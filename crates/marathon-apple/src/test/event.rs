//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/test/TestEvent.kt

use super::super::logparser::parser::DeviceFailureReason;
use jcompat::writers::KotlinValue;
use marathon_core::{error::VendorResult, model::Test, time::ReplayHooks};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestEvent {
    TestRunStartedEvent,
    TestStarted {
        id: Test,
    },
    TestFailed {
        id: Test,
        start_time: i64,
        end_time: i64,
        trace: Option<String>,
    },
    TestPassed {
        id: Test,
        start_time: i64,
        end_time: i64,
    },
    TestIgnored {
        id: Test,
        start_time: i64,
        end_time: i64,
    },
    TestRunFailed {
        message: String,
        reason: DeviceFailureReason,
    },
    TestRunEnded,
}
impl TestEvent {
    pub fn to_kotlin_string(&self, hooks: &dyn ReplayHooks) -> VendorResult<String> {
        use TestEvent::*;
        let (name, fields) = match self {
            TestStarted { id } => ("TestStarted", vec![("id".into(), id.kotlin_value(hooks)?)]),
            TestFailed {
                id,
                start_time,
                end_time,
                trace,
            } => {
                let mut fields = Self::timed(id, *start_time, *end_time, hooks)?;
                fields.push((
                    "trace".into(),
                    trace.clone().map_or(KotlinValue::Null, KotlinValue::Text),
                ));
                ("TestFailed", fields)
            }
            TestPassed {
                id,
                start_time,
                end_time,
            } => ("TestPassed", Self::timed(id, *start_time, *end_time, hooks)?),
            TestIgnored {
                id,
                start_time,
                end_time,
            } => ("TestIgnored", Self::timed(id, *start_time, *end_time, hooks)?),
            TestRunFailed { message, reason } => (
                "TestRunFailed",
                vec![
                    ("message".into(), KotlinValue::Text(message.to_owned())),
                    ("reason".into(), reason.name().into()),
                ],
            ),
            // These singleton events have no production rendering site.
            TestRunStartedEvent => return Ok("TestRunStartedEvent".into()),
            TestRunEnded => return Ok("TestRunEnded".into()),
        };
        Ok(KotlinValue::DataClass {
            name: name.into(),
            fields,
        }
        .render())
    }
    fn timed(id: &Test, start: i64, end: i64, hooks: &dyn ReplayHooks) -> VendorResult<Vec<(String, KotlinValue)>> {
        Ok(vec![
            ("id".into(), id.kotlin_value(hooks)?),
            ("startTime".into(), KotlinValue::Text(start.to_string())),
            ("endTime".into(), KotlinValue::Text(end.to_string())),
        ])
    }
}
