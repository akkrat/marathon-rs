//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/CompositeTestRunListener.kt
use super::AppleTestRunListener;
use crate::logparser::parser::DeviceFailureReason;

use marathon_core::{model::Test, runtime::VendorFuture};
use std::sync::Arc;
pub struct CompositeTestRunListener {
    pub listeners: Vec<Arc<dyn AppleTestRunListener>>,
}
impl CompositeTestRunListener {
    pub fn new(listeners: Vec<Arc<dyn AppleTestRunListener>>) -> Self {
        Self { listeners }
    }
}
impl AppleTestRunListener for CompositeTestRunListener {
    fn before_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.before_test_run().await?;
            }
            Ok(())
        })
    }
    fn test_run_started<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_run_started().await?;
            }
            Ok(())
        })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_started(test).await?;
            }
            Ok(())
        })
    }
    fn test_failed<'a>(
        &'a self,
        test: &'a Test,
        start_time: i64,
        end_time: i64,
        trace: Option<&'a str>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_failed(test, start_time, end_time, trace).await?;
            }
            Ok(())
        })
    }
    fn test_passed<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_passed(test, start_time, end_time).await?;
            }
            Ok(())
        })
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_ignored(test, start_time, end_time).await?;
            }
            Ok(())
        })
    }
    fn test_run_ended<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_run_ended().await?;
            }
            Ok(())
        })
    }
    fn test_run_failed<'a>(&'a self, error_message: &'a str, reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.test_run_failed(error_message, reason).await?;
            }
            Ok(())
        })
    }
    fn after_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            for listener in &self.listeners {
                listener.after_test_run().await?;
            }
            Ok(())
        })
    }
}
