//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/TestRunListenerAdapter.kt
use super::AppleTestRunListener;
use jcompat::writers::JavaString;
use marathon_core::listener::TestRunListener;
use marathon_core::{model::Test, runtime::VendorFuture};
use std::sync::Arc;
pub struct TestRunListenerAdapter {
    pub listener: Arc<dyn TestRunListener>,
}
impl AppleTestRunListener for TestRunListenerAdapter {
    fn before_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        self.listener.before_test_run()
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        self.listener.test_started(test)
    }
    fn test_failed<'a>(
        &'a self,
        test: &'a Test,
        _start_time: i64,
        _end_time: i64,
        _trace: Option<&'a JavaString>,
    ) -> VendorFuture<'a, ()> {
        self.listener.test_ended(test)
    }
    fn test_passed<'a>(&'a self, test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        self.listener.test_ended(test)
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        self.listener.test_ended(test)
    }
    fn after_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        self.listener.after_test_run()
    }
}
