//! Port of cli/src/main/resources/logback.xml and core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt.
//!
//! WP 8.8's console: one fmt layer (with a compact flag) over one queue writer.
//! Producers enqueue bytes; only the owned writer thread touches the sink.

use std::{
    fmt,
    io::{self, Write},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::{
    filter::Targets,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    reload,
};

static EXITING: AtomicBool = AtomicBool::new(false);

/// Ends the process once. The exit watchdog, the console writer and a second signal can each
/// reach `process::exit` at nearly the same time, and libc `exit` must not run on two threads
/// at once (exit handlers would run twice, or the process can hang). The first caller exits;
/// a later one parks until that exit completes.
pub fn exit_once(status: i32) -> ! {
    if EXITING.swap(true, Ordering::SeqCst) {
        loop {
            thread::park();
        }
    }
    std::process::exit(status)
}

pub use marathon_logging::{Vendor as Configurator, filter};

pub(crate) const EXIT_FLUSH: std::time::Duration = std::time::Duration::from_secs(1);
const ORDINARY_COMPLETION: Duration = Duration::from_secs(60);

struct CompletionDeadline {
    deadline: OnceLock<Instant>,
    timeout: Duration,
}
impl CompletionDeadline {
    fn deadline(&self) -> Instant {
        *self.deadline.get_or_init(|| Instant::now() + self.timeout)
    }
}

const OPEN: u8 = 0;
const DRAINING: u8 = 1;
const CLOSED: u8 = 2;

enum Command {
    Bytes(Vec<u8>),
    Flush(mpsc::Sender<io::Result<()>>),
    Finish,
    Exit(i32, mpsc::Sender<()>),
}
struct Queue {
    send: mpsc::Sender<Command>,
    phase: AtomicU8,
    active: AtomicUsize,
    accepted: AtomicUsize,
    written: AtomicUsize,
    rejected: AtomicUsize,
}
#[derive(Clone)]
pub struct QueueWriter(Arc<Queue>);
#[derive(Clone, Copy, Debug)]
pub struct OutputStats {
    pub accepted: usize,
    pub written: usize,
    pub rejected: usize,
}
impl QueueWriter {
    pub fn stats(&self) -> OutputStats {
        OutputStats {
            accepted: self.0.accepted.load(Ordering::SeqCst),
            written: self.0.written.load(Ordering::SeqCst),
            rejected: self.0.rejected.load(Ordering::SeqCst),
        }
    }
}
impl Write for QueueWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.active.fetch_add(1, Ordering::SeqCst);
        if self.0.phase.load(Ordering::SeqCst) == CLOSED {
            self.0.rejected.fetch_add(1, Ordering::SeqCst);
            self.0.active.fetch_sub(1, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "console is closed; late output rejected",
            ));
        }
        let result = self.0.send.send(Command::Bytes(bytes.to_vec()));
        if result.is_ok() {
            self.0.accepted.fetch_add(1, Ordering::SeqCst);
        } else {
            self.0.rejected.fetch_add(1, Ordering::SeqCst);
        }
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        result.map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console writer stopped"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Owned by the CLI. Joining and the drain deadline never run on core.
pub struct OutputGuard {
    writer: QueueWriter,
    thread: Option<JoinHandle<io::Result<()>>>,
    done: mpsc::Receiver<()>,
    completion: Arc<CompletionDeadline>,
}
impl OutputGuard {
    pub fn shutdown(mut self) -> io::Result<()> {
        self.finish(self.completion.deadline())
    }
    pub fn shutdown_with_timeout(mut self, timeout: std::time::Duration) -> io::Result<()> {
        self.finish((Instant::now() + timeout).min(self.completion.deadline()))
    }
    fn finish(&mut self, deadline: Instant) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        self.writer.0.phase.store(DRAINING, Ordering::SeqCst);
        let _ = self.writer.0.send.send(Command::Finish);
        if self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            let stats = self.writer.stats();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "console drain deadline elapsed (accepted {}, written {}, rejected {})",
                    stats.accepted, stats.written, stats.rejected
                ),
            ));
        }
        thread.join().map_err(|_| io::Error::other("console writer panicked"))?
    }
}
impl Drop for OutputGuard {
    fn drop(&mut self) {
        let _ = self.finish(self.completion.deadline());
    }
}

type FilterHandle = reload::Handle<Targets, tracing_subscriber::Registry>;
type ExitRequest = (i32, mpsc::Receiver<()>);
type ExitWork = (i32, mpsc::Sender<()>, Box<dyn FnOnce() + Send>);
#[derive(Clone)]
pub struct TracingOutput {
    dispatch: Dispatch,
    filter: FilterHandle,
    compact: Arc<AtomicBool>,
    writer: QueueWriter,
    exit: mpsc::Sender<ExitRequest>,
    prepare_exit: mpsc::Sender<ExitWork>,
    completion: Arc<CompletionDeadline>,
}
fn copy_error(error: &Option<io::Error>) -> io::Result<()> {
    error
        .as_ref()
        .map_or(Ok(()), |e| Err(io::Error::new(e.kind(), e.to_string())))
}
fn write_bytes(sink: &mut dyn Write, bytes: &[u8], queue: &Queue, failure: &mut Option<io::Error>) {
    match sink.write_all(bytes).and_then(|()| sink.flush()) {
        Ok(()) => {
            queue.written.fetch_add(1, Ordering::SeqCst);
        }
        Err(error) => {
            failure.get_or_insert(error);
        }
    }
}
impl TracingOutput {
    /// `stamp` renders the prefix time: `Some(wall_millis)` is the queued
    /// record's own wall time, `None` asks for now.
    pub fn new(
        sink: impl Write + Send + 'static,
        terminal: bool,
        stamp: impl Fn(Option<i64>) -> String + Send + Sync + 'static,
    ) -> io::Result<(Self, OutputGuard)> {
        Self::new_impl(sink, terminal, stamp, EXIT_FLUSH)
    }
    #[cfg(feature = "test-seams")]
    pub fn new_with_exit_flush(
        sink: impl Write + Send + 'static,
        terminal: bool,
        stamp: impl Fn(Option<i64>) -> String + Send + Sync + 'static,
        exit_flush: std::time::Duration,
    ) -> io::Result<(Self, OutputGuard)> {
        Self::new_impl(sink, terminal, stamp, exit_flush)
    }
    fn new_impl(
        sink: impl Write + Send + 'static,
        terminal: bool,
        stamp: impl Fn(Option<i64>) -> String + Send + Sync + 'static,
        exit_flush: std::time::Duration,
    ) -> io::Result<(Self, OutputGuard)> {
        let completion = Arc::new(CompletionDeadline {
            deadline: OnceLock::new(),
            timeout: ORDINARY_COMPLETION,
        });
        let (send, receive) = mpsc::channel();
        let writer = QueueWriter(Arc::new(Queue {
            send,
            phase: AtomicU8::new(OPEN),
            active: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
            written: AtomicUsize::new(0),
            rejected: AtomicUsize::new(0),
        }));
        let (exit, requests) = mpsc::channel::<ExitRequest>();
        std::thread::Builder::new()
            .name("marathon-exit".into())
            .spawn(move || {
                if let Ok((status, drained)) = requests.recv()
                    && drained.recv_timeout(exit_flush).is_err()
                {
                    exit_once(status);
                }
            })?;
        let (prepare_exit, preparation) = mpsc::channel::<ExitWork>();
        let exit_writer = writer.clone();
        std::thread::Builder::new()
            .name("marathon-exit-output".into())
            .spawn(move || {
                if let Ok((status, ack, prepare)) = preparation.recv() {
                    prepare();
                    let _ = exit_writer.0.send.send(Command::Exit(status, ack));
                }
            })?;
        let queue = writer.0.clone();
        let (done, completed) = mpsc::channel();
        let thread = thread::Builder::new().name("marathon-console".into()).spawn(move || {
            let mut sink = sink;
            let mut failure = None;
            let exit = loop {
                match receive.recv() {
                    Ok(Command::Bytes(bytes)) => write_bytes(&mut sink, &bytes, &queue, &mut failure),
                    Ok(Command::Flush(ack)) => {
                        let _ = ack.send(copy_error(&failure).and_then(|()| sink.flush()));
                    }
                    Ok(Command::Finish) => break None,
                    Ok(Command::Exit(status, ack)) => break Some((status, ack)),
                    Err(_) => break None,
                }
            };
            // Writers that observed OPEN/DRAINING must finish enqueueing before the last drain.
            queue.phase.store(CLOSED, Ordering::SeqCst);
            while queue.active.load(Ordering::SeqCst) != 0 {
                thread::park_timeout(std::time::Duration::from_millis(1));
            }
            for command in receive.try_iter() {
                match command {
                    Command::Bytes(bytes) => write_bytes(&mut sink, &bytes, &queue, &mut failure),
                    Command::Flush(ack) => {
                        let _ = ack.send(copy_error(&failure));
                    }
                    Command::Finish | Command::Exit(..) => {}
                }
            }
            if let Err(error) = sink.flush() {
                failure.get_or_insert(error);
            }
            let _ = done.send(());
            if let Some((status, ack)) = exit {
                let _ = ack.send(());
                exit_once(status);
            }
            failure.map_or(Ok(()), Err)
        })?;
        let compact = Arc::new(AtomicBool::new(false));
        let (layer, filter) = reload::Layer::new(filter(Configurator::Startup, true));
        let event_format = LineFormat {
            compact: compact.clone(),
            stamp: Arc::new(stamp),
        };
        let output_writer = writer.clone();
        let subscriber = tracing_subscriber::registry().with(layer).with(
            tracing_subscriber::fmt::layer()
                .event_format(event_format)
                .fmt_fields(NativeFields)
                .with_ansi(terminal)
                .with_writer(move || output_writer.clone()),
        );
        Ok((
            Self {
                dispatch: Dispatch::new(subscriber),
                filter,
                compact,
                writer: writer.clone(),
                exit,
                prepare_exit,
                completion: completion.clone(),
            },
            OutputGuard {
                writer,
                thread: Some(thread),
                done: completed,
                completion,
            },
        ))
    }
    #[cfg(feature = "test-seams")]
    pub fn completion_started(&self) -> bool {
        self.completion.deadline.get().is_some()
    }
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }
    pub fn writer(&self) -> QueueWriter {
        self.writer.clone()
    }
    pub fn flush(&self) -> io::Result<()> {
        // Ordinary completion starts here; the guard drains against this same deadline.
        let deadline = self.completion.deadline();
        let (send, receive) = mpsc::channel();
        self.writer
            .0
            .send
            .send(Command::Flush(send))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console writer stopped"))?;
        receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "console flush deadline elapsed"))?
    }
    /// Exits with `status` once everything queued is written. If the writer
    /// has already closed (the guard finished first), the `Exit` command has
    /// nowhere to go: its acknowledgement disconnects at once, so the exit
    /// watchdog fires immediately rather than after the flush deadline.
    pub fn exit_after_flush(&self, status: i32) -> io::Result<()> {
        self.exit_after_preparing(status, Box::new(|| {}))
    }
    pub fn exit_after_preparing(&self, status: i32, prepare: Box<dyn FnOnce() + Send>) -> io::Result<()> {
        self.writer.0.phase.store(DRAINING, Ordering::SeqCst);
        let (send, receive) = mpsc::channel();
        self.exit
            .send((status, receive))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "exit watchdog stopped"))?;
        self.prepare_exit
            .send((status, send, prepare))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "exit preparation stopped"))
    }
    pub fn configure(&self, configurator: Configurator, debug: bool) -> Result<(), reload::Error> {
        self.filter.reload(filter(configurator, debug))?;
        self.compact.store(
            matches!(configurator, Configurator::Apple { compact_output: true }),
            Ordering::Relaxed,
        );
        Ok(())
    }
}

/// Field formatting for the native lines: values print raw (never
/// Debug-quoted), so a multi-line message or stack trace keeps its bytes and
/// whitespace on its continuation lines. The `message` field prints without
/// its name, every other field as `name=value`, in declaration order. The
/// `wall_millis` field of a queued record is the line's timestamp, not output.
struct NativeFields;
struct NativeVisitor<'a> {
    writer: Writer<'a>,
    first: bool,
    result: fmt::Result,
}
impl NativeVisitor<'_> {
    fn write_field(&mut self, field: &tracing::field::Field, value: fmt::Arguments<'_>) {
        if self.result.is_err() || field.name() == "wall_millis" {
            return;
        }
        self.result = (|| {
            if !self.first {
                write!(self.writer, " ")?;
            }
            self.first = false;
            if field.name() != "message" {
                write!(self.writer, "{}=", field.name())?;
            }
            write!(self.writer, "{value}")
        })();
    }
}
impl tracing::field::Visit for NativeVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.write_field(field, format_args!("{value}"));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.write_field(field, format_args!("{value:?}"));
    }
}
impl<'a> FormatFields<'a> for NativeFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(&self, writer: Writer<'_>, fields: R) -> fmt::Result {
        let mut visitor = NativeVisitor {
            writer,
            first: true,
            result: Ok(()),
        };
        fields.record(&mut visitor);
        visitor.result
    }
}

/// Reads a queued record's own wall time off the event, so the line is
/// stamped when the record was created, however late it drains.
struct WallVisitor(Option<i64>);
impl tracing::field::Visit for WallVisitor {
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if field.name() == "wall_millis" {
            self.0 = Some(value);
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn fmt::Debug) {}
}

struct LineFormat {
    compact: Arc<AtomicBool>,
    stamp: Arc<dyn Fn(Option<i64>) -> String + Send + Sync>,
}
impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, context: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        if !self.compact.load(Ordering::Relaxed) {
            let mut wall = WallVisitor(None);
            event.record(&mut wall);
            write!(writer, "{} ", (self.stamp)(wall.0))?;
        }
        let level = event.metadata().level();
        if writer.has_ansi_escapes() {
            let colour = match *level {
                tracing::Level::ERROR => 31,
                tracing::Level::WARN => 33,
                tracing::Level::INFO => 32,
                _ => 36,
            };
            write!(writer, "\x1b[{colour}m{level}\x1b[0m ")?;
        } else {
            write!(writer, "{level} ")?;
        }
        write!(writer, "{}: ", event.metadata().target())?;
        context.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    struct Stalled {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        stopped: mpsc::Sender<()>,
    }
    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.release
                .recv_timeout(Duration::from_secs(60))
                .map_err(io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Stalled {
        fn drop(&mut self) {
            let _ = self.stopped.send(());
        }
    }

    #[test]
    fn stalled_flush_and_drain_share_one_deadline_u196() -> io::Result<()> {
        let (entered, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (stopped, finished) = mpsc::channel();
        let (output, guard) = TracingOutput::new(
            Stalled {
                entered,
                release: blocked,
                stopped,
            },
            false,
            |_| "time".into(),
        )?;
        output.writer().write_all(b"queued")?;
        waiting
            .recv_timeout(Duration::from_secs(60))
            .map_err(io::Error::other)?;
        // Consume the ordinary allowance deterministically, without waiting a real minute.
        output
            .completion
            .deadline
            .set(Instant::now() - Duration::from_secs(1))
            .map_err(|_| io::Error::other("completion already started"))?;
        let error = output
            .flush()
            .err()
            .ok_or_else(|| io::Error::other("flush did not expire"))?;
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "console flush deadline elapsed");
        // The sink stays gated through both stages. Flush used all of the shared allowance;
        // draining must get the same expired deadline, rather than a fresh timeout.
        assert_eq!(guard.completion.deadline(), output.completion.deadline());
        assert_eq!(
            guard.completion.deadline().saturating_duration_since(Instant::now()),
            Duration::ZERO
        );
        let (drained, drain) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            let _ = drained.send(guard.shutdown());
        });
        // Deliberate expiry: a fresh 60-second budget must fail this bound, with
        // ample scheduling margin compared with an already expired wait.
        let result = drain.recv_timeout(Duration::from_secs(5));
        release.send(()).map_err(io::Error::other)?;
        shutdown.join().map_err(|_| io::Error::other("shutdown panicked"))?;
        let error = result
            .map_err(io::Error::other)?
            .err()
            .ok_or_else(|| io::Error::other("drain did not expire"))?;
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error.to_string(),
            "console drain deadline elapsed (accepted 1, written 0, rejected 0)"
        );
        finished
            .recv_timeout(Duration::from_secs(60))
            .map_err(io::Error::other)?;
        Ok(())
    }
}
