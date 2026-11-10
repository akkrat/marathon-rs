//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/test/TestEvent.kt

use super::super::logparser::parser::DeviceFailureReason;
use marathon_core::model::Test;

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
