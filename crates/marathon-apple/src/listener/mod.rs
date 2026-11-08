//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/AppleTestRunListener.kt
use crate::logparser::parser::DeviceFailureReason;

use marathon_core::{model::Test, runtime::VendorFuture};

pub mod accumulating;
pub mod adapter;
pub mod artifact;
pub mod composite;
pub mod debug;
pub mod result_bundle_run_listener;
pub mod results;
pub use accumulating::AccumulatingTestResultListener;
pub use adapter::TestRunListenerAdapter;
pub use artifact::{ArtifactFinderListener, DiagnosticLogsPathFinder, SessionResultsPathFinder};
pub use composite::CompositeTestRunListener;
pub use debug::DebugTestRunListener;
pub use result_bundle_run_listener::ResultBundleRunListener;
pub use results::{TestResultsListener, TestResultsListenerConfig};

pub trait AppleTestRunListener: Send + Sync {
    fn before_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_run_started<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_started<'a>(&'a self, _test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_failed<'a>(
        &'a self,
        _test: &'a Test,
        _start_time: i64,
        _end_time: i64,
        _trace: Option<&'a String>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_passed<'a>(&'a self, _test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_ignored<'a>(&'a self, _test: &'a Test, _start_time: i64, _end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_run_ended<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn test_run_failed<'a>(&'a self, _error_message: &'a String, _reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn after_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
