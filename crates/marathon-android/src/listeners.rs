//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/AndroidTestRunListener.kt,
//! AccumulatingResultTestRunListener.kt, CompositeTestRunListener.kt, DebugTestRunListener.kt,
//! NoOpTestRunListener.kt, TestResultsListener.kt and TestRunListenerAdapter.kt.
use crate::{models::TestIdentifier, support::*};
use jcompat::{logging::Level, regex_patterns::PARAMETERIZED_METHOD};
use marathon_adb::testrunner::{TestEvent, TestIdentifier as AdamIdentifier};
use marathon_core::{
    artifacts::AttachmentCollector,
    error::*,
    execution::result::{TemporalResults, TestRunResultsAccumulator},
    listener::{AttachmentProvider, TestRunListener},
    logging::LogRecord,
    model::*,
    runtime::*,
    vendor::DeviceRef,
};
use marathon_dex::apk_parser::InstrumentationInfo;
use marathon_error::FailureKind;
use std::sync::{Arc, Mutex};

/// Event values retain adam's order; run-start uses the runner's fixed AdamTestRun name.
pub trait AndroidTestRunListener: Send + Sync {
    /// Explicit final ownership boundary, including exceptional and cancelled exits.
    fn cleanup_in(&self, _context: marathon_core::vendor::VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    /// Suspending vendor listeners use the timeout body's explicit child context.
    /// Context-free adapters remain useful for synchronous accumulation and core listeners.
    fn before_test_run_in<'a>(
        &'a self,
        info: Option<&'a InstrumentationInfo>,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        self.before_test_run(info)
    }
    /// D28: the batch's next instrumentation is about to start; `before_test_run_in` told of the first. Not called for
    /// a batch of one instrumentation.
    fn before_next_instrumentation_in<'a>(
        &'a self,
        _info: &'a InstrumentationInfo,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// D28: the batch's next instrumentation, first or later, is about to run these tests, before it clears any data or
    /// gets as far as `before_test_run_in`. Not called for a batch of one instrumentation.
    fn on_instrumentation_tests_in<'a>(
        &'a self,
        _tests: &'a [Test],
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    /// D25: the instrumentation's stream failed, for this reason; a `TestRunFailed` with the same text follows.
    fn on_stream_failed_in<'a>(
        &'a self,
        _reason: &'a str,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn on_event_in<'a>(
        &'a self,
        event: &'a TestEvent,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        self.on_event(event)
    }
    fn after_test_run_in(&self, _context: marathon_core::vendor::VendorContext) -> VendorFuture<'_, ()> {
        self.after_test_run()
    }

    fn before_test_run<'a>(&'a self, _info: Option<&'a InstrumentationInfo>) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn on_event<'a>(&'a self, _event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub struct NoOpTestRunListener;
impl AndroidTestRunListener for NoOpTestRunListener {}
pub struct CompositeTestRunListener {
    pub listeners: Vec<Arc<dyn AndroidTestRunListener>>,
}
impl AndroidTestRunListener for CompositeTestRunListener {
    fn cleanup_in(&self, context: marathon_core::vendor::VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let mut error = None;
            for listener in &self.listeners {
                if let Err(e) = listener.cleanup_in(context.clone()).await {
                    error.get_or_insert(e);
                }
            }
            error.map_or(Ok(()), Err)
        })
    }

    fn before_test_run_in<'a>(
        &'a self,
        info: Option<&'a InstrumentationInfo>,
        context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.before_test_run_in(info, context.clone()).await?;
            }
            Ok(())
        })
    }
    fn before_next_instrumentation_in<'a>(
        &'a self,
        info: &'a InstrumentationInfo,
        context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.before_next_instrumentation_in(info, context.clone()).await?;
            }
            Ok(())
        })
    }
    fn on_instrumentation_tests_in<'a>(
        &'a self,
        tests: &'a [Test],
        context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.on_instrumentation_tests_in(tests, context.clone()).await?;
            }
            Ok(())
        })
    }
    fn on_stream_failed_in<'a>(
        &'a self,
        reason: &'a str,
        context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.on_stream_failed_in(reason, context.clone()).await?;
            }
            Ok(())
        })
    }
    fn on_event_in<'a>(
        &'a self,
        event: &'a TestEvent,
        context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !matches!(event, TestEvent::TestRunFailing { .. }) {
                for listener in &self.listeners {
                    listener.on_event_in(event, context.clone()).await?;
                }
            }
            Ok(())
        })
    }
    fn after_test_run_in(&self, context: marathon_core::vendor::VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.after_test_run_in(context.clone()).await?;
            }
            Ok(())
        })
    }

    fn before_test_run<'a>(&'a self, info: Option<&'a InstrumentationInfo>) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.before_test_run(info).await?;
            }
            Ok(())
        })
    }
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            // N2: CompositeTestRunListener never overrides testRunFailing.
            if !matches!(event, TestEvent::TestRunFailing { .. }) {
                for listener in &self.listeners {
                    listener.on_event(event).await?;
                }
            }
            Ok(())
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.after_test_run().await?;
            }
            Ok(())
        })
    }
}
pub fn to_test(id: &AdamIdentifier) -> Test {
    TestIdentifier {
        class_name: id.class_name.clone(),
        test_name: id.test_name.clone(),
    }
    .to_test(vec![])
}
pub struct TestRunListenerAdapter {
    pub listener: Arc<dyn TestRunListener>,
}
impl AndroidTestRunListener for TestRunListenerAdapter {
    fn before_test_run<'a>(&'a self, _: Option<&'a InstrumentationInfo>) -> VendorFuture<'a, ()> {
        self.listener.before_test_run()
    }
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            match event {
                TestEvent::TestStarted { id } => self.listener.test_started(&to_test(id)).await,
                TestEvent::TestEnded { id, .. } => self.listener.test_ended(&to_test(id)).await,
                _ => Ok(()),
            }
        })
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        self.listener.after_test_run()
    }
}
pub type ListenerLog = Arc<dyn Fn(LogRecord) + Send + Sync>;
pub struct AccumulatingResultTestRunListener {
    pub result: Mutex<TestRunResultsAccumulator>,
    pub logs: ListenerLog,
}
impl AccumulatingResultTestRunListener {
    pub fn update<T>(
        &self,
        body: impl FnOnce(&mut TestRunResultsAccumulator) -> marathon_core::time::ClockResult<T>,
    ) -> VendorResult<T> {
        let (result, logs) = {
            let mut accumulator = lock(&self.result);
            let result = body(&mut accumulator);
            (result, accumulator.take_logs())
        };
        for record in logs {
            (self.logs)(record);
        }
        result.map_err(|e| other(FailureKind::ReplayClock, e.message))
    }
}
impl AndroidTestRunListener for AccumulatingResultTestRunListener {
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.update(|a| {
                match event {
                    TestEvent::TestRunStartedEvent { test_count } => {
                        a.test_run_started("AdamTestRun".into(), *test_count)
                    }
                    TestEvent::TestStarted { id } => a.test_started(&to_test(id), None)?,
                    TestEvent::TestFailed { id, stack_trace } => {
                        a.test_failed(&to_test(id), stack_trace, None, None)?
                    }
                    TestEvent::TestAssumptionFailed { id, stack_trace } => {
                        a.test_assumption_failure(&to_test(id), stack_trace)?
                    }
                    TestEvent::TestIgnored { id } => a.test_ignored(&to_test(id))?,
                    TestEvent::TestEnded { id, metrics } => {
                        a.test_ended(&to_test(id), metrics.entries().to_vec(), None, None)?
                    }
                    TestEvent::TestRunFailed { error } => a.test_run_failed(error.clone())?,
                    TestEvent::TestRunStopped { elapsed_time_millis } => a.test_run_stopped(*elapsed_time_millis)?,
                    TestEvent::TestRunEnded {
                        elapsed_time_millis,
                        metrics,
                    } => a.test_run_ended(*elapsed_time_millis, metrics.entries().to_vec())?,
                    TestEvent::TestRunFailing { .. } => {}
                }
                Ok(())
            })
        })
    }
}
pub struct TestResultsListener {
    pub accumulator: AccumulatingResultTestRunListener,
    pub batch: BatchRef,
    pub device: DeviceRef,
    pub deferred: Arc<dyn Deferred<TestBatchResults>>,
    pub attachments: Arc<AttachmentCollector>,
    pub log: Arc<dyn Fn(Level, String) + Send + Sync>,
}
impl TestResultsListener {
    pub fn collector(providers: &[Arc<dyn AttachmentProvider>]) -> VendorResult<Arc<AttachmentCollector>> {
        AttachmentCollector::new(providers)
    }
    pub fn merge_parameterised_results(&self, results: &mut TemporalResults) -> TemporalResults {
        let mut merged = TemporalResults::default();
        let explicit = self
            .batch
            .tests
            .iter()
            .any(|t| t.method.contains('[') && t.method.contains(']'));
        let mut origins: Vec<(Test, Test)> = vec![];
        for (test, result) in results.entries().to_vec() {
            let mut key = test.clone();
            if !explicit && PARAMETERIZED_METHOD.matches(&test.method.to_owned()) {
                key.method.truncate(key.method.find('[').unwrap_or(key.method.len()));
                key.meta_properties.clear();
                if let Some(old) = merged.get_mut(&key) {
                    old.status = old.status + result.status;
                    // Kotlin's merged value aliases the first temporal record.
                    if let Some((_, origin)) = origins.iter().find(|(merged, _)| *merged == key)
                        && let Some(original) = results.get_mut(origin)
                    {
                        original.status = old.status;
                    }
                    continue;
                }
            }
            if let Some((_, origin)) = origins.iter_mut().find(|(merged, _)| *merged == key) {
                *origin = test;
            } else {
                origins.push((key.clone(), test));
            }
            merged.insert(key, result);
        }
        merged
    }
}
impl AndroidTestRunListener for TestResultsListener {
    /// D28: the next run's start must not erase why this one failed, which is the text of its own uncompleted tests.
    fn on_instrumentation_tests_in<'a>(
        &'a self,
        tests: &'a [Test],
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.accumulator.update(|a| {
                a.start_instrumentation(tests.to_vec());
                Ok(())
            })
        })
    }
    fn on_stream_failed_in<'a>(
        &'a self,
        reason: &'a str,
        _context: marathon_core::vendor::VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.accumulator.update(|a| {
                a.explain_tests_in_progress(reason);
                Ok(())
            })
        })
    }
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        self.accumulator.on_event(event)
    }
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let mut finished = vec![];
            let mut failed = vec![];
            let mut uncompleted = vec![];
            self.accumulator.update(|a| {
                let merged = self.merge_parameterised_results(&mut a.temporal_test_results);
                for (key, value) in merged.entries() {
                    // N1: Kotlin compares the requested clazz to key.pkg, losing metadata normally.
                    let test = self
                        .batch
                        .tests
                        .iter()
                        .find(|t| t.pkg == key.pkg && t.clazz == key.pkg && t.method == key.method)
                        .unwrap_or(key);
                    let result = TestResult {
                        test: test.clone(),
                        device: self.device.0.info(),
                        test_batch_id: self.batch.id.clone(),
                        status: value.status,
                        start_time: value.start_time,
                        end_time: value.end_time,
                        stacktrace: value.stack_trace.clone(),
                        attachments: self.attachments.get(key).unwrap_or_default(),
                    };
                    if key.method == "null" && !self.batch.tests.contains(key) {
                        continue;
                    }
                    if value.is_successful() {
                        finished.push(result);
                    } else if value.status == TestStatus::Incomplete {
                        uncompleted.push(result);
                    } else {
                        failed.push(result);
                    }
                }
                let missing: Vec<_> = self
                    .batch
                    .tests
                    .iter()
                    .filter(|t| merged.get(t).is_none())
                    .cloned()
                    .collect();
                uncompleted.extend(a.create_uncompleted_test_results(&missing, &self.batch, &self.device.0.info())?);
                Ok(())
            })?;
            for result in &uncompleted {
                let mut text = String::from("uncompleted = ");
                text.push_str(&result.test.to_test_name('.', '#'));
                text.push_str(", ");
                text.push_str(&self.device.0.info().serial_number);
                (self.log)(Level::Warn, text);
            }
            self.deferred.complete(Ok(TestBatchResults {
                device: self.device.clone(),
                finished,
                failed,
                uncompleted,
            }));
            Ok(())
        })
    }
}
pub struct DebugTestRunListener {
    pub device: crate::device::AndroidDevice,
}
impl AndroidTestRunListener for DebugTestRunListener {
    fn on_event<'a>(&'a self, event: &'a TestEvent) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let serial = self.device.serial_number();
            let identifier = |id: &AdamIdentifier| {
                format!(
                    "TestIdentifier(className={}, testName={})",
                    id.class_name.to_owned(),
                    id.test_name.to_owned()
                )
            };
            let s = match event {
                TestEvent::TestRunStartedEvent { .. } => format!("testRunStarted {}", serial.to_owned()),
                TestEvent::TestStarted { id } => {
                    format!("testStarted {} test = {}", serial.to_owned(), identifier(id))
                }
                TestEvent::TestFailed { id, stack_trace } => format!(
                    "testFailed {} test = {} trace = {}",
                    serial.to_owned(),
                    identifier(id),
                    stack_trace.to_owned()
                ),
                TestEvent::TestAssumptionFailed { id, stack_trace } => format!(
                    "testAssumptionFailure {} test = {} trace = {}",
                    serial.to_owned(),
                    identifier(id),
                    stack_trace.to_owned()
                ),
                TestEvent::TestIgnored { id } => {
                    format!("testIgnored {} test = {}", serial.to_owned(), identifier(id))
                }
                TestEvent::TestEnded { id, .. } => {
                    format!("testEnded {} test = {}", serial.to_owned(), identifier(id))
                }
                TestEvent::TestRunFailed { error } => format!(
                    "testRunFailed {} errorMessage = {}",
                    serial.to_owned(),
                    error.to_owned()
                ),
                TestEvent::TestRunFailing { error, stack_trace } => format!(
                    "testRunFailing {} errorMessage = {} trace = {}",
                    serial.to_owned(),
                    error.to_owned(),
                    stack_trace.to_owned()
                ),
                TestEvent::TestRunStopped { elapsed_time_millis } => format!(
                    "testRunStopped {} elapsedTime = {elapsed_time_millis}",
                    serial.to_owned()
                ),
                TestEvent::TestRunEnded {
                    elapsed_time_millis, ..
                } => format!("testRunEnded elapsedTime {elapsed_time_millis}"),
            };
            self.device.log_named("DebugTestRunListener", Level::Info, &s, None);
            Ok(())
        })
    }
}
