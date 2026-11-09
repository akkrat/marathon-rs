//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (shared stdout).
use crate::application::Console;
use marathon_logging::LogSettings;
use marathon_core::{logging::OutputRecord, output::ConsoleSink, runtime::VendorFuture};
use std::sync::{Arc, Mutex};
pub struct RunConsoleSink { console: Arc<Console> }
impl RunConsoleSink {
    pub fn new(console: Arc<Console>, _: Arc<Mutex<LogSettings>>, _: Box<dyn Fn(i64) -> i64 + Send + Sync>) -> Self { Self { console } }
}
impl ConsoleSink for RunConsoleSink {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            for record in records { match record { OutputRecord::Raw(text) => self.console.write_text(&text), OutputRecord::Log(record) => self.console.write_record(&record) } }
            Ok(())
        })
    }
}
