//! Port of cli/src/main/kotlin/com/malinskiy/marathon/cli/ApplicationView.kt (the shared stdout),
//! as a recording sink for tests.
//!
//! Resolves loggers as the CLI's `RunConsoleSink` does: a name already registered keeps its
//! level; any other name is created at its first rendered record, with the registry's debug
//! state at that moment (C18).
use crate::lock;
use jcompat::{
    logging::{Level, Logger, MarathonLogging},
    writers::JavaString,
};
use marathon_core::{error::Failure, logging::OutputRecord, output::ConsoleSink, runtime::VendorFuture};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

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
    registry: Arc<Mutex<MarathonLogging>>,
    loggers: Mutex<HashMap<String, Logger>>,
    lines: Mutex<Vec<Line>>,
    /// Record count of every `write` call: the flush points, as seen by the sink.
    batches: Mutex<Vec<usize>>,
}
impl RecordingConsole {
    pub fn new(registry: Arc<Mutex<MarathonLogging>>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            loggers: Mutex::default(),
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
                OutputRecord::Raw(text) => lock(&self.lines).push(Line::Raw(text.to_string_lossy())),
                OutputRecord::Log(record) => {
                    let enabled = {
                        let mut registry = lock(&self.registry);
                        let mut loggers = lock(&self.loggers);
                        let logger = loggers.entry(record.logger.clone()).or_insert_with(|| {
                            if registry.is_registered(&record.logger) {
                                registry.slf4j_logger(&record.logger)
                            } else {
                                registry.logger(&record.logger, None)
                            }
                        });
                        registry.enabled(logger, record.level)
                    };
                    if enabled {
                        lock(&self.lines).push(Line::Log {
                            logger: record.logger,
                            level: record.level,
                            message: record.message.to_string_lossy(),
                            cause: record.cause,
                        });
                    }
                }
            }
        }
        Box::pin(async { Ok(()) })
    }
}

/// Text of a JavaString for assertions.
pub fn text(value: &JavaString) -> String {
    value.to_string_lossy()
}
