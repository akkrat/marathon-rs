//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/DebugLogPrinter.kt
use crate::cmd::remote::ssh::sshj::config::SshLogging;
use marathon_logging::{Level, Component};
use marathon_core::{listener::LineListener, runtime::VendorFuture};
use std::sync::OnceLock;

pub struct DebugLogPrinter {
    prefix: String,
    hide_runner_output: bool,
    logging: SshLogging,
    logger: OnceLock<Component>,
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
    fn on_line<'a>(&'a self, line: &'a str) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if !self.hide_runner_output {
                let logger = self.logger.get_or_init(|| {
                    let logger = self.logging.component(&self.prefix);
                    self.logging.log(
                        &logger,
                        Level::TRACE,
                        || format!("Mirroring remote logs with name '{}'", self.prefix),
                        None,
                    );
                    logger
                });
                self.logging.log(logger, Level::DEBUG, || line.to_owned(), None);
            }
            Ok(())
        })
    }
}
