//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/logparser/XctestEventProducer.kt
//! and logparser/TestEventProducer.kt.
use super::parser::{DeviceFailureParser, ParseError, TestRunProgressParser};
use crate::test::TestEvent;

pub struct XctestEventProducer {
    pub progress: TestRunProgressParser,
}
impl XctestEventProducer {
    pub fn new(progress: TestRunProgressParser) -> Self {
        Self { progress }
    }
    pub fn process(&mut self, line: &str) -> Result<Option<Vec<TestEvent>>, ParseError> {
        if let Some(events) = DeviceFailureParser.process(line)? {
            return Ok(Some(events));
        }
        self.progress.process(line)
    }
}
