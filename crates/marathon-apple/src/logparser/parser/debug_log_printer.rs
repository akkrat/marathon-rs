//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/DebugLogPrinter.kt
use crate::cmd::remote::ssh::sshj::config::SshLogging;
use jcompat::{
    logging::{Level, Logger},
    writers::JavaString,
};
use marathon_core::{listener::LineListener, runtime::VendorFuture};
use std::sync::OnceLock;

pub struct DebugLogPrinter {
    prefix: String,
    hide_runner_output: bool,
    logging: SshLogging,
    logger: OnceLock<Logger>,
}
impl DebugLogPrinter {
    pub fn new(prefix: Option<String>, hide_runner_output: bool, logging: SshLogging) -> Self {
        Self {
            prefix: prefix.unwrap_or_else(|| "DebugLogPrinter".into()),
            hide_runner_output,
            logging,
            logger: OnceLock::new(),
        }
    }
}
impl LineListener for DebugLogPrinter {
    fn on_line<'a>(&'a self, line: &'a JavaString) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.hide_runner_output {
                let logger = self.logger.get_or_init(|| {
                    let logger = self.logging.logger(&self.prefix);
                    self.logging.log(
                        &logger,
                        Level::Trace,
                        || format!("Mirroring remote logs with name '{}'", self.prefix),
                        None,
                    );
                    logger
                });
                self.logging.log(logger, Level::Debug, || line.to_string_lossy(), None);
            }
            Ok(())
        })
    }
}
