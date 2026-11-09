//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! and cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (shared stdout).
use crate::application::Console;
use jcompat::logging::{LogEvent, Logger, MarathonLogging};
use marathon_core::{logging::OutputRecord, output::ConsoleSink, runtime::VendorFuture};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

/// Construct with the startup Console and registry; OutputDriver calls it off core.
/// The zone function returns the local offset at each event's wall timestamp.
pub struct RunConsoleSink {
    console: Arc<Console>,
    registry: Arc<Mutex<MarathonLogging>>,
    loggers: Mutex<HashMap<String, Logger>>,
    zone: Box<dyn Fn(i64) -> i64 + Send + Sync>,
}
impl RunConsoleSink {
    pub fn new(
        console: Arc<Console>,
        registry: Arc<Mutex<MarathonLogging>>,
        zone: Box<dyn Fn(i64) -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            console,
            registry,
            loggers: Mutex::default(),
            zone,
        }
    }
    fn write_records(&self, records: Vec<OutputRecord>) {
        for record in records {
            match record {
                OutputRecord::Raw(text) => self.console.write_text(&text),
                OutputRecord::Log(record) => {
                    let (enabled, layout, name) = {
                        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
                        let mut loggers = self.loggers.lock().unwrap_or_else(PoisonError::into_inner);
                        // Loggers registered elsewhere (Timings, the metrics providers) keep
                        // their levels: re-requesting them would reset INFO/DEBUG to ERROR.
                        let logger = loggers.entry(record.logger.clone()).or_insert_with(|| {
                            if registry.is_registered(&record.logger) {
                                registry.slf4j_logger(&record.logger)
                            } else {
                                registry.logger(&record.logger, None)
                            }
                        });
                        (
                            registry.enabled(logger, record.level),
                            registry.layout(),
                            logger.name().to_owned(),
                        )
                    };
                    if enabled {
                        let cause = record.cause.map(|cause| cause.stack_trace());
                        self.console.write_event(
                            layout,
                            &LogEvent {
                                level: record.level,
                                logger: &name,
                                thread: std::thread::current().name().unwrap_or("main"),
                                local_time_millis: record.wall_millis.wrapping_add((self.zone)(record.wall_millis)),
                                message: &record.message,
                                throwable: cause.as_deref().unwrap_or(""),
                            },
                        );
                    }
                }
            }
        }
    }
}
impl ConsoleSink for RunConsoleSink {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.write_records(records);
            Ok(())
        })
    }
}
