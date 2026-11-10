//! Port of cli/src/main/resources/logback.xml and core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt.
//!
//! WP 8.8's design pilot. Producers enqueue bytes; only the owned writer thread touches the sink.

use std::{
    fmt,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::{
    filter::Targets,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    reload,
};

pub use marathon_logging::{Vendor as Configurator, filter};

const EXIT_FLUSH: std::time::Duration = std::time::Duration::from_secs(1);
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
}
impl OutputGuard {
    pub fn shutdown(self) -> io::Result<()> {
        self.shutdown_with_timeout(EXIT_FLUSH)
    }
    pub fn shutdown_with_timeout(mut self, timeout: std::time::Duration) -> io::Result<()> {
        self.finish(timeout)
    }
    fn finish(&mut self, timeout: std::time::Duration) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        self.writer.0.phase.store(DRAINING, Ordering::SeqCst);
        let _ = self.writer.0.send.send(Command::Finish);
        if self.done.recv_timeout(timeout).is_err() {
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
        let _ = self.finish(EXIT_FLUSH);
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
    pub fn new(
        sink: impl Write + Send + 'static,
        terminal: bool,
        clock: impl Fn() -> String + Send + Sync + 'static,
    ) -> io::Result<(Self, OutputGuard)> {
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
                    && drained.recv_timeout(EXIT_FLUSH).is_err()
                {
                    std::process::exit(status);
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
                std::process::exit(status);
            }
            failure.map_or(Ok(()), Err)
        })?;
        let compact = Arc::new(AtomicBool::new(false));
        let (layer, filter) = reload::Layer::new(filter(Configurator::Startup, true));
        let event_format = LineFormat {
            compact: compact.clone(),
            clock: Arc::new(clock),
        };
        let output_writer = writer.clone();
        let subscriber = tracing_subscriber::registry().with(layer).with(
            tracing_subscriber::fmt::layer()
                .event_format(event_format)
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
            },
            OutputGuard {
                writer,
                thread: Some(thread),
                done: completed,
            },
        ))
    }
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }
    pub fn writer(&self) -> QueueWriter {
        self.writer.clone()
    }
    pub fn flush(&self) -> io::Result<()> {
        let (send, receive) = mpsc::channel();
        self.writer
            .0
            .send
            .send(Command::Flush(send))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console writer stopped"))?;
        receive
            .recv_timeout(EXIT_FLUSH)
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "console flush deadline elapsed"))?
    }
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

struct LineFormat {
    compact: Arc<AtomicBool>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
}
impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, context: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        if !self.compact.load(Ordering::Relaxed) {
            write!(writer, "{} ", (self.clock)())?;
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
