//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (the shared stdout),
//! as a recording sink for tests.
//!
//! Filters records as the CLI's subscriber does, through the registry's current
//! configurator filter (`LogSettings::enabled_target`).
use crate::lock;
use marathon_core::{error::Failure, logging::OutputRecord, output::ConsoleSink, runtime::VendorFuture};
use marathon_logging::{Level, LogSettings};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Log {
        logger: String,
        level: Level,
        message: String,
        cause: Option<Failure>,
    },
    Raw(String),
}

pub struct RecordingConsole {
    registry: Arc<Mutex<LogSettings>>,
    lines: Mutex<Vec<Line>>,
    /// Record count of every `write` call: the flush points, as seen by the sink.
    batches: Mutex<Vec<usize>>,
}
impl RecordingConsole {
    pub fn new(registry: Arc<Mutex<LogSettings>>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            lines: Mutex::default(),
            batches: Mutex::default(),
        })
    }
    /// Sizes of the batches handed to the sink, in write order.
    pub fn batch_sizes(&self) -> Vec<usize> {
        lock(&self.batches).clone()
    }
    pub fn lines(&self) -> Vec<Line> {
        lock(&self.lines).clone()
    }
    /// Messages of the enabled records of one logger, in output order.
    pub fn messages(&self, logger: &str) -> Vec<String> {
        self.lines()
            .into_iter()
            .filter_map(|line| match line {
                Line::Log {
                    logger: name, message, ..
                } if name == logger => Some(message),
                _ => None,
            })
            .collect()
    }
    pub fn raw(&self) -> String {
        self.lines()
            .into_iter()
            .filter_map(|line| match line {
                Line::Raw(text) => Some(text),
                _ => None,
            })
            .collect()
    }
}
impl ConsoleSink for RecordingConsole {
    fn write(&self, records: Vec<OutputRecord>) -> VendorFuture<'_, ()> {
        lock(&self.batches).push(records.len());
        for record in records {
            match record {
                OutputRecord::Raw(text) => lock(&self.lines).push(Line::Raw(text.to_owned())),
                OutputRecord::Log(record) => {
                    let enabled = lock(&self.registry).enabled_target(record.target(), record.level);
                    if enabled {
                        lock(&self.lines).push(Line::Log {
                            logger: record.logger,
                            level: record.level,
                            message: record.message.to_owned(),
                            cause: record.cause,
                        });
                    }
                }
            }
        }
        Box::pin(async { Ok(()) })
    }
}

/// Text of a String for assertions.
pub fn text(value: &str) -> String {
    value.to_owned()
}
