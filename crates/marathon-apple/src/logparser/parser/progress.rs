//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/parser/TestRunProgressParser.kt
use marathon_logging::{Level, Component};

use crate::{bin::kt, cmd::remote::ssh::sshj::config::SshLogging, test::TestEvent};
use jcompat::{
    jnum, jtime,
    
    regex_patterns::*,
};
use marathon_core::{
    model::Test,
    time::{ReplayError, Timer},
};
use marathon_error::FailureKind;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    NumberFormat { input: String },
    Clock(ReplayError),
}
impl ParseError {
    pub fn to_failure(&self) -> marathon_core::error::Failure {
        let (class, message) = match self {
            Self::NumberFormat { input } => (
                FailureKind::NumberFormat,
                if input.bytes().filter(|b| *b == b'.').count() > 1 {
                    "multiple points".into()
                } else {
                    format!("For input string: \"{input}\"")
                },
            ),
            Self::Clock(error) => (FailureKind::ReplayClock, error.message.to_owned()),
        };
        crate::cmd::failure(class, message)
    }
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_failure().fmt(f)
    }
}
impl std::error::Error for ParseError {}

pub struct TestRunProgressParser {
    timer: Arc<dyn Timer>,
    target_name: String,
    failing_trace: Option<String>,
    logging: SshLogging,
    logger: Component,
}
impl TestRunProgressParser {
    pub fn new(timer: Arc<dyn Timer>, target_name: impl Into<String>, logging: SshLogging) -> Self {
        let logger = logging.component("marathon_apple::logparser::parser::progress");
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
                Level::TRACE,
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
                Level::DEBUG,
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
            let allowance = Allowance::read(&c[3]);
            let test = self.test(&c[1], &c[2]);
            self.logging.log(
                &self.logger,
                Level::DEBUG,
                || {
                    format!(
                        "Test {} finished with result <timeout> after {} seconds",
                        log_name(&test),
                        allowance.seconds
                    )
                },
                None,
            );
            if !allowance.understood {
                self.logging.log(
                    &self.logger,
                    Level::WARN,
                    || {
                        format!(
                            "Execution time allowance '{}' of test {} is not understood, taking it as 0 seconds",
                            &c[3],
                            log_name(&test)
                        )
                    },
                    None,
                );
            }
            let end_time = self.timer.current_time_millis().map_err(ParseError::Clock)?;
            TestEvent::TestFailed {
                id: test,
                start_time: end_time.wrapping_sub(allowance.millis),
                end_time,
                // D45: this line is what's known about a hang, and Kotlin reported nothing.
                trace: self.failing_trace.clone().or_else(|| Some(line.into())),
            }
        } else {
            if FAILING_TEST.matches(line)
                && let Some(c) = FAILING_TEST.find(line)
            {
                self.failing_trace = Some(format!("{}:{}", kt::trim(&c[1]), kt::trim(&c[4])));
            }
            return Ok(None);
        };
        Ok(Some(vec![event]))
    }
}

/// D45: the allowance only dates the failure, so no text may stop a hung test from being reported.
/// Kotlin's `parseAppleDuration` threw on `1 minute`, which XCTest prints for 60 s (A47).
struct Allowance {
    seconds: i64,
    millis: i64,
    understood: bool,
}
impl Allowance {
    fn read(text: &str) -> Self {
        // Everything Kotlin could read stays as it was, including its odd forms.
        let iso = format!(
            "PT{}",
            text.replace(" seconds", "S")
                .replace(" minutes", "M")
                .replace(" hours", "H")
        );
        if let Some(duration) = jtime::parse_duration(&iso)
            && let Some(millis) = duration.to_millis()
        {
            return Self {
                seconds: duration.seconds(),
                millis,
                understood: true,
            };
        }
        match spoken_millis(text) {
            Some(millis) => Self {
                seconds: millis / 1000,
                millis,
                understood: true,
            },
            None => Self {
                seconds: 0,
                millis: 0,
                understood: false,
            },
        }
    }
}

/// `1 minute`, `1 hour, 1 minute`, `1.5 minutes`: the English forms of NSDateComponentsFormatter.
/// XCTest formats the allowance with the test process's locale, so other languages can reach the caller's fallback.
fn spoken_millis(text: &str) -> Option<i64> {
    let mut tokens = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|token| !token.is_empty() && *token != "and");
    let mut total = 0_f64;
    let mut read = false;
    while let Some(number) = tokens.next() {
        let value = number.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)?;
        let scale = match tokens.next()?.to_ascii_lowercase().as_str() {
            "second" | "seconds" | "sec" | "secs" => 1_000.0,
            "minute" | "minutes" | "min" | "mins" => 60_000.0,
            "hour" | "hours" | "hr" | "hrs" => 3_600_000.0,
            "day" | "days" => 86_400_000.0,
            "week" | "weeks" | "wk" | "wks" => 604_800_000.0,
            _ => return None,
        };
        total += value * scale;
        read = true;
    }
    (read && total < i64::MAX as f64).then_some(total.round() as i64)
}

fn log_name(test: &Test) -> String {
    format!(
        "{}.{}.{}",
        test.pkg.to_owned(),
        test.clazz.to_owned(),
        test.method.to_owned()
    )
}
