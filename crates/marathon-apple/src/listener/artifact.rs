//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/listener/ArtifactFinderListener.kt
//! and logparser/parser/{DiagnosticLogsPathFinder,SessionResultsPathFinder}.kt.
use super::AppleTestRunListener;
use crate::{bin::kt, cmd::remote::ssh::sshj::config::SshLogging};
use jcompat::{
    JavaRegex,
    logging::{Level, Logger},
    regex_patterns::{DIAGNOSTIC_LOGS_PATH, SESSION_RESULTS_PATH, find_artifact},
    writers::JavaString,
};
use marathon_core::{listener::LineListener, runtime::VendorFuture};
use std::sync::Mutex;

pub struct ArtifactFinderListener {
    pattern: JavaRegex,
    message: String,
    matches: Mutex<Vec<String>>,
    logging: SshLogging,
    logger: Logger,
}
impl ArtifactFinderListener {
    pub fn new(pattern: JavaRegex, message: impl Into<String>, logging: SshLogging) -> Self {
        let logger = logging.logger("com.malinskiy.marathon.apple.listener.ArtifactFinderListener");
        Self {
            pattern,
            message: message.into(),
            matches: Mutex::new(vec![]),
            logging,
            logger,
        }
    }
}
impl LineListener for ArtifactFinderListener {
    fn on_line<'a>(&'a self, line: &'a JavaString) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if let Some(path) = find_artifact(&self.pattern, &line.to_string_lossy()) {
                let path = kt::trim(path).to_owned();
                let mut matches = self.matches.lock().unwrap_or_else(|e| e.into_inner());
                if !matches.contains(&path) {
                    matches.push(path);
                }
            }
            Ok(())
        })
    }
}
impl AppleTestRunListener for ArtifactFinderListener {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let paths = self.matches.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if !paths.is_empty() {
                self.logging.log(
                    &self.logger,
                    Level::Info,
                    || format!("{} {}", self.message, paths.join(", ")),
                    None,
                );
            }
            Ok(())
        })
    }
}
pub struct DiagnosticLogsPathFinder(pub ArtifactFinderListener);
impl DiagnosticLogsPathFinder {
    pub fn new(logging: SshLogging) -> Self {
        Self(ArtifactFinderListener::new(
            DIAGNOSTIC_LOGS_PATH.clone(),
            "Diagnostic logs available at",
            logging,
        ))
    }
}
impl LineListener for DiagnosticLogsPathFinder {
    fn on_line<'a>(&'a self, line: &'a JavaString) -> VendorFuture<'a, ()> {
        self.0.on_line(line)
    }
}
impl AppleTestRunListener for DiagnosticLogsPathFinder {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        self.0.after_test_run()
    }
}
pub struct SessionResultsPathFinder(pub ArtifactFinderListener);
impl SessionResultsPathFinder {
    pub fn new(logging: SshLogging) -> Self {
        Self(ArtifactFinderListener::new(
            SESSION_RESULTS_PATH.clone(),
            "Session results available at",
            logging,
        ))
    }
}
impl LineListener for SessionResultsPathFinder {
    fn on_line<'a>(&'a self, line: &'a JavaString) -> VendorFuture<'a, ()> {
        self.0.on_line(line)
    }
}
impl AppleTestRunListener for SessionResultsPathFinder {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        self.0.after_test_run()
    }
}
