//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt
//! (the logger calls of core actors; docs/design/lifecycle.md, "Core log records").
//!
//! Core may not do I/O or take the registry lock, so actors append structured
//! records here and the run's output task sends them through tracing off core.

use crate::error::Failure;
use crossbeam_queue::SegQueue;
use marathon_logging::Level;
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// One `logger.<level>(cause) { message }` call. The message is already
/// interpolated: Kotlin's lazy message lambdas in core have no side effects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    pub target: String,
    pub logger: String,
    /// Scheduler time of the call: replay order and timelines.
    pub at_millis: i64,
    /// Wall time (epoch millis) of the call, stamped when the record is created;
    /// replay order uses it however late the output task drains.
    pub wall_millis: i64,
    pub level: Level,
    pub message: String,
    pub cause: Option<Failure>,
}

/// Per-run, core-thread queue, in call order across all core actors.
pub struct LogQueue {
    records: RefCell<Vec<OutputRecord>>,
    /// Every producer, core or Send, enqueues here, so push order is the one
    /// output order; reads drain it into `records` first.
    incoming: Arc<SegQueue<OutputRecord>>,
    wall: RefCell<Rc<dyn Fn() -> i64>>,
    late: LateOutput,
    output_tail: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Signalled by every push; only a live run's output pump waits on it.
    wake: Arc<tokio::sync::Notify>,
    /// A live output pump also takes batches, so an empty `serial_output` must still wait for them.
    concurrent_output: std::cell::Cell<bool>,
}
impl Default for LogQueue {
    fn default() -> Self {
        let incoming: Arc<SegQueue<OutputRecord>> = Arc::default();
        Self {
            late: LateOutput::new(incoming.clone()),
            records: RefCell::default(),
            incoming,
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
    pub fn late_output(&self) -> LateOutput {
        self.late.clone()
    }
    pub(crate) fn detach_output(&self) {
        let pending = std::mem::take(&mut *self.records.borrow_mut());
        if !pending.is_empty() {
            self.late.head.push(pending);
        }
    }
    #[track_caller]
    pub fn log(&self, logger: &str, at_millis: i64, level: Level, message: String, cause: Option<Failure>) {
        let wall_millis = (self.wall.borrow())();
        self.late.push(OutputRecord::Log(LogRecord {
            // The producing module, not this queue's: a record whose logger is
            // not itself a module path is filtered and printed by its caller.
            target: marathon_logging::module_target(std::panic::Location::caller().file()),
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
            wake: self.wake.clone(),
            late: self.late.clone(),
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
    Raw(String),
}
impl LogQueue {
    pub fn raw(&self, text: String) {
        self.late.push(OutputRecord::Raw(text));
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

/// Send producer for the same run queue. The caller stamps the record at its
/// call site; rendering and registry access still happen off core.
#[derive(Clone)]
pub struct LogSender {
    late: LateOutput,
    wake: Arc<tokio::sync::Notify>,
}
impl LogSender {
    pub fn push(&self, record: LogRecord) {
        self.late.push(OutputRecord::Log(record));
        self.wake.notify_one();
    }
}

impl LogRecord {
    /// The event target: the logger itself when it is a module path, the owning
    /// module for the remaining named loggers, else the construction site's
    /// module (see `LogQueue::log`).
    pub fn target(&self) -> &str {
        if self.logger.starts_with("marathon_") {
            &self.logger
        } else if self.logger == "FilterValuesFileReader" {
            "marathon_core::filter::values"
        } else if self.logger.contains("DeviceActor[") {
            "marathon_core::execution::device::actor"
        } else if self.logger.starts_with("DevicePoolActor[") {
            "marathon_core::execution::pool"
        } else if self.logger.starts_with("QueueActor[") {
            "marathon_core::execution::queue"
        } else {
            &self.target
        }
    }
    pub fn emit(&self) {
        let cause = self.cause.as_ref().map(|failure| failure.for_log().stack_trace());
        marathon_logging::emit(
            self.target(),
            self.level,
            &self.message,
            cause.as_deref(),
            (self.target() != self.logger).then_some(self.logger.as_str()),
            self.wall_millis,
        );
    }
}

/// Hands the unbounded queue to an off-core forwarding task during final shutdown.
#[derive(Clone)]
pub struct LateOutput {
    queue: Arc<SegQueue<OutputRecord>>,
    head: Arc<SegQueue<Vec<OutputRecord>>>,
    sender: Arc<std::sync::OnceLock<std::sync::mpsc::Sender<LateRecord>>>,
    active: Arc<std::sync::atomic::AtomicUsize>,
    setup: Arc<std::sync::Mutex<()>>,
}
enum LateRecord {
    Output(OutputRecord),
    Fence(std::sync::mpsc::Sender<()>),
}
impl LateOutput {
    fn new(queue: Arc<SegQueue<OutputRecord>>) -> Self {
        Self {
            queue,
            head: Arc::default(),
            sender: Arc::default(),
            active: Arc::default(),
            setup: Arc::default(),
        }
    }
    fn push(&self, record: OutputRecord) {
        use std::sync::atomic::Ordering;
        self.active.fetch_add(1, Ordering::SeqCst);
        if let Some(sender) = self.sender.get() {
            // The forwarder lives for as long as any producer. A failed send is reported explicitly.
            if sender.send(LateRecord::Output(record)).is_err() {
                self.active.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        } else {
            self.queue.push(record);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    /// Off core. The fence includes the buffered tail and all forwarded events submitted before it.
    pub fn close(&self, sink: Arc<dyn Fn(OutputRecord) + Send + Sync>) -> std::io::Result<()> {
        use std::sync::{atomic::Ordering, mpsc};
        let _setup = self.setup.lock().unwrap_or_else(|e| e.into_inner());
        if self.sender.get().is_none() {
            let (sender, receive) = mpsc::channel();
            let (start, prefix) = mpsc::channel::<Vec<OutputRecord>>();
            std::thread::Builder::new()
                .name("marathon-late-output".into())
                .spawn(move || {
                    if let Ok(records) = prefix.recv() {
                        for record in records {
                            sink(record);
                        }
                    }
                    for record in receive {
                        match record {
                            LateRecord::Output(record) => sink(record),
                            LateRecord::Fence(done) => {
                                let _ = done.send(());
                            }
                        }
                    }
                })?;
            let _ = self.sender.set(sender);
            while self.active.load(Ordering::SeqCst) != 0 {
                std::thread::park_timeout(std::time::Duration::from_millis(1));
            }
            let mut records = Vec::new();
            while let Some(batch) = self.head.pop() {
                records.extend(batch);
            }
            while let Some(record) = self.queue.pop() {
                records.push(record);
            }
            start
                .send(records)
                .map_err(|_| std::io::Error::other("late output forwarder stopped"))?;
        }
        let (send, receive) = mpsc::channel();
        let Some(sender) = self.sender.get() else {
            return Err(std::io::Error::other("late output forwarder missing"));
        };
        sender
            .send(LateRecord::Fence(send))
            .map_err(|_| std::io::Error::other("late output forwarder stopped"))?;
        receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "late output fence deadline elapsed"))
    }
}
