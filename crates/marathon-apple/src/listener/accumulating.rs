//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/AccumulatingTestResultListener.kt
use super::AppleTestRunListener;
use crate::logparser::parser::DeviceFailureReason;
use jcompat::writers::JavaString;
use marathon_core::{
    error::{VendorError, VendorResult},
    execution::result::{Metrics, TestRunResultsAccumulator},
    logging::LogSender,
    time::ClockResult,
};
use marathon_core::{model::Test, runtime::VendorFuture};
use std::sync::Mutex;

pub struct AccumulatingTestResultListener {
    pub(crate) result: Mutex<TestRunResultsAccumulator>,
    expected_test_count: i32,
    logs: LogSender,
}
impl AccumulatingTestResultListener {
    pub fn new(expected_test_count: i32, result: TestRunResultsAccumulator, logs: LogSender) -> Self {
        Self {
            result: Mutex::new(result),
            expected_test_count,
            logs,
        }
    }
    pub(crate) fn update<T>(
        &self,
        f: impl FnOnce(&mut TestRunResultsAccumulator) -> ClockResult<T>,
    ) -> VendorResult<T> {
        let mut a = self.result.lock().unwrap_or_else(|e| e.into_inner());
        let result = f(&mut a);
        for record in a.take_logs() {
            self.logs.push(record);
        }
        result.map_err(|e| VendorError::Other(crate::cmd::failure("ReplayError", e.message.to_string_lossy())))
    }
}
impl AppleTestRunListener for AccumulatingTestResultListener {
    fn test_run_started<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.update(|a| {
                a.test_run_started("apple".into(), self.expected_test_count);
                Ok(())
            })
        })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.update(|a| a.test_started(test, None)) })
    }
    fn test_failed<'a>(
        &'a self,
        test: &'a Test,
        start_time: i64,
        end_time: i64,
        trace: Option<&'a JavaString>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.update(|a| {
                a.test_failed(
                    test,
                    trace.unwrap_or(&JavaString::default()),
                    Some(start_time),
                    Some(end_time),
                )?;
                a.test_ended(test, Metrics::default(), None, None)
            })
        })
    }
    fn test_passed<'a>(&'a self, test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.update(|a| a.test_ended(test, Metrics::default(), None, None)) })
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            self.update(|a| {
                a.test_ignored(test)?;
                a.test_ended(test, Metrics::default(), None, None)
            })
        })
    }
    fn test_run_ended<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.update(|a| a.test_run_ended(0, Metrics::default())) })
    }
    fn test_run_failed<'a>(
        &'a self,
        error_message: &'a JavaString,
        _reason: DeviceFailureReason,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.update(|a| a.test_run_failed(error_message.clone())) })
    }
}
