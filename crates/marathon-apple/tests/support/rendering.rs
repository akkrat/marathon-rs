//! Port of core/src/main/kotlin/com/malinskiy/marathon/test/Test.kt and
//! vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/test/TestEvent.kt.

use marathon_apple::test::TestEvent;
use marathon_core::{model::Test, report::projection::metadata_text};

// Historical parser/listener witnesses use generated data-class strings.
// Keep that adapter in tests, independently of the production diagnostic format.
pub fn test_text(test: &Test) -> String {
    let properties = test
        .meta_properties
        .iter()
        .map(|p| {
            let values = p
                .values
                .entries()
                .iter()
                .map(|(key, value)| format!("{key}={}", metadata_text(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("MetaProperty(name={}, values={{{values}}})", p.name)
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Test(pkg={}, clazz={}, method={}, metaProperties=[{properties}])",
        test.pkg, test.clazz, test.method
    )
}

pub fn event_text(event: &TestEvent) -> String {
    use TestEvent::*;
    match event {
        TestRunStartedEvent => "TestRunStartedEvent".into(),
        TestRunEnded => "TestRunEnded".into(),
        TestStarted { id } => format!("TestStarted(id={})", test_text(id)),
        TestFailed {
            id,
            start_time,
            end_time,
            trace,
        } => format!(
            "TestFailed(id={}, startTime={start_time}, endTime={end_time}, trace={})",
            test_text(id),
            trace.as_deref().unwrap_or("null")
        ),
        TestPassed {
            id,
            start_time,
            end_time,
        } => format!(
            "TestPassed(id={}, startTime={start_time}, endTime={end_time})",
            test_text(id)
        ),
        TestIgnored {
            id,
            start_time,
            end_time,
        } => format!(
            "TestIgnored(id={}, startTime={start_time}, endTime={end_time})",
            test_text(id)
        ),
        TestRunFailed { message, reason } => format!("TestRunFailed(message={message}, reason={})", reason.name()),
    }
}
