//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/DebugTestRunListener.kt
use super::AppleTestRunListener;
use crate::cmd::remote::ssh::sshj::config::SshLogging;
use crate::logparser::parser::DeviceFailureReason;
use jcompat::logging::{Level, Logger};

use marathon_core::vendor::DeviceRef;
use marathon_core::{model::Test, runtime::VendorFuture};
pub struct DebugTestRunListener {
    device: DeviceRef,
    logging: SshLogging,
    logger: Logger,
}
impl DebugTestRunListener {
    pub fn new(device: DeviceRef, logging: SshLogging) -> Self {
        let logger = logging.logger("DebugTestRunListener");
        Self {
            device,
            logging,
            logger,
        }
    }
    fn log(&self, message: String) {
        self.logging.log(&self.logger, Level::Info, || message, None);
    }
}
impl AppleTestRunListener for DebugTestRunListener {
    fn test_run_started<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testRunStarted {}",
                self.device.0.info().serial_number.to_owned()
            ));
            Ok(())
        })
    }
    fn test_started<'a>(&'a self, test: &'a Test) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testStarted {} test = {}",
                self.device.0.info().serial_number.to_owned(),
                test.kotlin_value().render().to_owned()
            ));
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
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testFailed {} test = {}, timespan = [{start_time},{end_time}], trace = \n{}",
                self.device.0.info().serial_number.to_owned(),
                test.kotlin_value().render().to_owned(),
                trace.map_or_else(|| "null".into(), str::to_owned)
            ));
            Ok(())
        })
    }
    fn test_passed<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testPassed {} test = {}, timespan = [{start_time},{end_time}]",
                self.device.0.info().serial_number.to_owned(),
                test.kotlin_value().render().to_owned()
            ));
            Ok(())
        })
    }
    fn test_ignored<'a>(&'a self, test: &'a Test, start_time: i64, end_time: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testIgnored {} test = {}, timespan = [{start_time},{end_time}]",
                self.device.0.info().serial_number.to_owned(),
                test.kotlin_value().render().to_owned()
            ));
            Ok(())
        })
    }
    fn test_run_ended<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log("testRunEnded".into());
            Ok(())
        })
    }
    fn test_run_failed<'a>(&'a self, error_message: &'a str, _reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log(format!(
                "testRunFailed {} errorMessage = {}",
                self.device.0.info().serial_number.to_owned(),
                error_message.to_owned()
            ));
            Ok(())
        })
    }
    fn after_test_run<'a>(&'a self) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.logging.enabled(&self.logger, Level::Info) {
                return Ok(());
            }
            self.log("afterTestRun".into());
            Ok(())
        })
    }
}
