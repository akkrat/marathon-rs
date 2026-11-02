//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/TestRunProgressParser.kt
use crate::{bin::kt, cmd::remote::ssh::sshj::config::SshLogging, test::TestEvent};
use jcompat::{
    jnum, jtime,
    logging::{Level, Logger},
    regex_patterns::*,
    writers::JavaString,
};
use marathon_core::{
    model::Test,
    time::{ReplayError, Timer},
};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    NumberFormat { input: String },
    Duration { input: String },
    DurationOverflow,
    Clock(ReplayError),
}
impl ParseError {
    pub fn to_failure(&self) -> marathon_core::error::Failure {
        let (class, message) = match self {
            Self::NumberFormat { input } => (
                "java.lang.NumberFormatException",
                if input.bytes().filter(|b| *b == b'.').count() > 1 {
                    "multiple points".into()
                } else {
                    format!("For input string: \"{input}\"")
                },
            ),
            Self::Duration { .. } => (
                "java.time.format.DateTimeParseException",
                "Text cannot be parsed to a Duration".into(),
            ),
            Self::DurationOverflow => ("java.lang.ArithmeticException", "long overflow".into()),
            Self::Clock(error) => ("ReplayError", error.message.to_string_lossy()),
        };
        crate::cmd::failure(class, message)
    }
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ParseError {}

pub struct TestRunProgressParser {
    timer: Arc<dyn Timer>,
    target_name: String,
    failing_trace: Option<JavaString>,
    logging: SshLogging,
    logger: Logger,
}
impl TestRunProgressParser {
    pub fn new(timer: Arc<dyn Timer>, target_name: impl Into<String>, logging: SshLogging) -> Self {
        let logger = logging.logger("TestRunProgressParser");
        Self {
            timer,
            target_name: target_name.into(),
            failing_trace: None,
            logging,
            logger,
        }
    }
    fn test(&self, class: &str, method: &str) -> Test {
        let (pkg, clazz) = match (class.rsplit_once('.'), class.split_once('.')) {
            (Some((pkg, _)), Some((_, clazz))) => (pkg, clazz),
            _ => (self.target_name.as_str(), class),
        };
        Test {
            pkg: pkg.into(),
            clazz: clazz.into(),
            method: method.into(),
            meta_properties: vec![],
        }
    }
    pub fn process(&mut self, line: &str) -> Result<Option<Vec<TestEvent>>, ParseError> {
        let event = if TEST_CASE_STARTED.matches(line) {
            self.failing_trace = None;
            let Some(c) = TEST_CASE_STARTED.find(line) else {
                return Ok(None);
            };
            let test = self.test(&c[1], &c[2]);
            self.logging.log(
                &self.logger,
                Level::Trace,
                || format!("Test {} started", log_name(&test)),
                None,
            );
            TestEvent::TestStarted { id: test }
        } else if TEST_CASE_FINISHED.matches(line) {
            let Some(c) = TEST_CASE_FINISHED.find(line) else {
                return Ok(None);
            };
            let duration = c[4]
                .parse::<f32>()
                .map_err(|_| ParseError::NumberFormat { input: c[4].into() })?;
            let test = self.test(&c[1], &c[2]);
            self.logging.log(
                &self.logger,
                Level::Debug,
                || {
                    format!(
                        "Test {} finished with result <{}> after {} seconds",
                        log_name(&test),
                        &c[3],
                        jnum::float_to_string(duration)
                    )
                },
                None,
            );
            let end_time = self.timer.current_time_millis().map_err(ParseError::Clock)?;
            let millis = jnum::kotlin_round_to_int(duration * 1000.0)
                .ok_or_else(|| ParseError::NumberFormat { input: c[4].into() })?;
            let start_time = end_time.wrapping_sub(i64::from(millis));
            match &c[3] {
                "passed" => TestEvent::TestPassed {
                    id: test,
                    start_time,
                    end_time,
                },
                "skipped" => TestEvent::TestIgnored {
                    id: test,
                    start_time,
                    end_time,
                },
                _ => TestEvent::TestFailed {
                    id: test,
                    start_time,
                    end_time,
                    trace: self.failing_trace.clone(),
                },
            }
        } else if TIMEOUT_TEST.matches(line) {
            let Some(c) = TIMEOUT_TEST.find(line) else {
                return Ok(None);
            };
            let iso = format!(
                "PT{}",
                c[3].replace(" seconds", "S")
                    .replace(" minutes", "M")
                    .replace(" hours", "H")
            );
            let duration = jtime::parse_duration(&iso).ok_or_else(|| ParseError::Duration { input: iso.clone() })?;
            let millis = duration.to_millis().ok_or(ParseError::DurationOverflow)?;
            let test = self.test(&c[1], &c[2]);
            self.logging.log(
                &self.logger,
                Level::Debug,
                || {
                    format!(
                        "Test {} finished with result <timeout> after {} seconds",
                        log_name(&test),
                        duration.seconds()
                    )
                },
                None,
            );
            let end_time = self.timer.current_time_millis().map_err(ParseError::Clock)?;
            TestEvent::TestFailed {
                id: test,
                start_time: end_time.wrapping_sub(millis),
                end_time,
                trace: self.failing_trace.clone(),
            }
        } else {
            if FAILING_TEST.matches(line)
                && let Some(c) = FAILING_TEST.find(line)
            {
                self.failing_trace = Some(format!("{}:{}", kt::trim(&c[1]), kt::trim(&c[4])).into());
            }
            return Ok(None);
        };
        Ok(Some(vec![event]))
    }
}

fn log_name(test: &Test) -> String {
    format!(
        "{}.{}.{}",
        test.pkg.to_string_lossy(),
        test.clazz.to_string_lossy(),
        test.method.to_string_lossy()
    )
}
