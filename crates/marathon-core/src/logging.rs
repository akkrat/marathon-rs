//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt
//! (the logger calls of core actors; docs/design/lifecycle.md, "Core log records").
//!
//! Core may not do I/O or take the registry lock, so actors append structured
//! records here and the run's output task renders them off core with jcompat.

use crate::error::Failure;
use crossbeam_queue::SegQueue;
use jcompat::{
    logging::{JansiMode, Level, LogEvent, Logger, MarathonLogging, OutputTarget},
    writers::JavaString,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// One `logger.<level>(cause) { message }` call. The message is already
/// interpolated: Kotlin's lazy message lambdas in core have no side effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    pub logger: String,
    /// Scheduler time of the call: replay order and timelines.
    pub at_millis: i64,
    /// Wall time (epoch millis) of the call, as logback stamps the event when it
    /// is created; the rendered timestamp, however late the output task drains.
    pub wall_millis: i64,
    pub level: Level,
    pub message: JavaString,
    pub cause: Option<Failure>,
}

/// Per-run, core-thread queue, in call order across all core actors.
pub struct LogQueue {
    records: RefCell<Vec<OutputRecord>>,
    /// Every producer, core or Send, enqueues here, so push order is the one
    /// output order; reads drain it into `records` first.
    incoming: Arc<SegQueue<OutputRecord>>,
    wall: RefCell<Rc<dyn Fn() -> i64>>,
    output_tail: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Signalled by every push; only a live run's output pump waits on it.
    wake: Arc<tokio::sync::Notify>,
    /// A live output pump also takes batches, so an empty `serial_output` must still wait for them.
    concurrent_output: std::cell::Cell<bool>,
}
impl Default for LogQueue {
    fn default() -> Self {
        Self {
            records: RefCell::default(),
            incoming: Arc::default(),
            wall: RefCell::new(Rc::new(host_millis)),
            output_tail: RefCell::default(),
            wake: Arc::default(),
            concurrent_output: std::cell::Cell::new(false),
        }
    }
}
/// A Send wall source for owners outside core, such as vendor-side accumulators.
pub type WallSource = std::sync::Arc<dyn Fn() -> i64 + Send + Sync>;
/// A clock read, not I/O, so core may take it per record.
pub fn host_millis() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}
impl LogQueue {
    pub fn log(&self, logger: &str, at_millis: i64, level: Level, message: JavaString, cause: Option<Failure>) {
        let wall_millis = (self.wall.borrow())();
        self.incoming.push(OutputRecord::Log(LogRecord {
            logger: logger.to_owned(),
            at_millis,
            wall_millis,
            level,
            message,
            cause,
        }));
        self.wake.notify_one();
    }
    /// Replaces the host wall clock, e.g. with a replay clock.
    pub fn set_wall_clock(&self, wall: Rc<dyn Fn() -> i64>) {
        *self.wall.borrow_mut() = wall;
    }
    pub fn sender(&self) -> LogSender {
        LogSender {
            queue: self.incoming.clone(),
            wake: self.wake.clone(),
        }
    }
    /// Pushes since the last wait wake the next one (a stored permit, so none is lost).
    pub(crate) fn wake(&self) -> Arc<tokio::sync::Notify> {
        self.wake.clone()
    }
    /// A live run's output pump flushes concurrently with the serial owners (see `serial_output`).
    pub(crate) fn set_concurrent_output(&self) {
        self.concurrent_output.set(true);
    }
    fn drain(&self) {
        let mut records = self.records.borrow_mut();
        while let Some(record) = self.incoming.pop() {
            records.push(record);
        }
    }
    pub fn records(&self) -> Vec<LogRecord> {
        self.drain();
        self.records
            .borrow()
            .iter()
            .filter_map(|record| match record {
                OutputRecord::Log(log) => Some(log.clone()),
                OutputRecord::Raw(_) => None,
            })
            .collect()
    }
    /// Hand the queued records to the output task.
    pub fn take(&self) -> Vec<LogRecord> {
        self.drain();
        let mut logs = Vec::new();
        self.records.borrow_mut().retain(|record| match record {
            OutputRecord::Log(log) => {
                logs.push(log.clone());
                false
            }
            OutputRecord::Raw(_) => true,
        });
        logs
    }
}

/// Raw println text and log events share the process console in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputRecord {
    Log(LogRecord),
    Raw(JavaString),
}
impl LogQueue {
    pub fn raw(&self, text: JavaString) {
        self.incoming.push(OutputRecord::Raw(text));
        self.wake.notify_one();
    }
    /// Reserve delivery order on core before vendor tasks can race to start.
    ///
    /// With the live pump running, a serial owner's records may already be in the pump's
    /// batch; its flush must still not return before that batch is written, so an empty take
    /// with an unfinished predecessor becomes an empty batch that only waits for it.
    pub(crate) fn serial_output(&self) -> Option<OutputBatch> {
        let records = self.take_output();
        if records.is_empty() && !(self.concurrent_output.get() && self.output_pending()) {
            return None;
        }
        let (done, next) = tokio::sync::oneshot::channel();
        let previous = self.output_tail.replace(Some(next));
        Some(OutputBatch {
            records,
            previous,
            done: OutputDone(Some(done)),
        })
    }
    fn output_pending(&self) -> bool {
        use tokio::sync::oneshot::error::TryRecvError;
        let mut tail = self.output_tail.borrow_mut();
        match tail.as_mut().map(|tail| tail.try_recv()) {
            Some(Err(TryRecvError::Empty)) => true,
            // A receiver that has resolved must not be awaited again.
            Some(_) => {
                *tail = None;
                false
            }
            None => false,
        }
    }
    /// Use this rather than `take` when raw and formatted output share a run.
    pub fn take_output(&self) -> Vec<OutputRecord> {
        self.drain();
        std::mem::take(&mut self.records.borrow_mut())
    }
}

pub(crate) struct OutputBatch {
    pub records: Vec<OutputRecord>,
    pub previous: Option<tokio::sync::oneshot::Receiver<()>>,
    pub done: OutputDone,
}
pub(crate) struct OutputDone(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for OutputDone {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            let _ = done.send(());
        }
    }
}

impl LogRecord {
    /// Off core: `logger` is the registry handle for `self.logger`. For already
    /// registered loggers (Timings, MetricsProviderImpl, NoOpMetricsProvider,
    /// MetricsProviderFactory), reuse the handle or resolve with `slf4j_logger`.
    /// Calling `MarathonLogging::logger(name, None)` again resets their levels.
    /// Only previously unregistered names are created once by the output task.
    /// `zone_offset_millis` is the local zone's offset at `wall_millis`.
    pub fn render(
        &self,
        registry: &MarathonLogging,
        logger: &Logger,
        thread: &str,
        zone_offset_millis: i64,
        output: (JansiMode, OutputTarget),
    ) -> Option<String> {
        registry.enabled(logger, self.level).then(|| {
            let throwable = self.cause.as_ref().map(|c| c.stack_trace().to_string_lossy());
            registry.layout().format(
                &LogEvent {
                    level: self.level,
                    logger: logger.name(),
                    thread,
                    local_time_millis: self.wall_millis.wrapping_add(zone_offset_millis),
                    message: &self.message.to_string_lossy(),
                    throwable: throwable.as_deref().unwrap_or(""),
                },
                output.0,
                output.1,
            )
        })
    }
}

/// Send producer for the same run queue. The caller stamps the record at its
/// call site; rendering and registry access still happen off core.
#[derive(Clone)]
pub struct LogSender {
    queue: Arc<SegQueue<OutputRecord>>,
    wake: Arc<tokio::sync::Notify>,
}
impl LogSender {
    pub fn push(&self, record: LogRecord) {
        self.queue.push(OutputRecord::Log(record));
        self.wake.notify_one();
    }
}
