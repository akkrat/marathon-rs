//! Port of cli/src/main/resources/logback.xml and core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt.
//!
//! WP 8.8's design pilot. Producers enqueue bytes; only the owned writer thread touches the sink.

use std::{
    fmt,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
use tracing::{Dispatch, Event, Subscriber, level_filters::LevelFilter};
use tracing_subscriber::{
    filter::Targets,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    reload,
};

#[derive(Clone, Copy, Debug)]
pub enum Configurator {
    Startup,
    Android,
    Apple { compact: bool },
}

/// Stable Rust targets replace logger registration order and inherited JVM package levels.
pub fn filter(configurator: Configurator, debug: bool) -> Targets {
    let ordinary = if debug || matches!(configurator, Configurator::Startup) {
        LevelFilter::DEBUG
    } else {
        LevelFilter::ERROR
    };
    let mut targets = Targets::new()
        .with_default(LevelFilter::ERROR)
        .with_targets(
            [
                "marathon_cli",
                "marathon_core",
                "marathon_config",
                "marathon_android",
                "marathon_apple",
                "marathon_ios",
                "marathon_macos",
                "marathon_analytics",
                "marathon_dex",
                "marathon_test_support",
            ]
            .map(|target| (target, ordinary)),
        )
        .with_target("marathon_cli::application", LevelFilter::DEBUG)
        .with_target("marathon_adb", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::factory", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::provider", LevelFilter::DEBUG)
        .with_target("marathon_core::analytics::noop", LevelFilter::DEBUG)
        .with_target("marathon_core::reporters::timeline", LevelFilter::DEBUG)
        .with_target("marathon_apple::cmd::remote::ssh::sshj::wire", LevelFilter::DEBUG)
        .with_target("marathon_core::timing", LevelFilter::INFO)
        .with_target("marathon_ios::device::timing", LevelFilter::INFO)
        .with_target("marathon_apple::xcodebuild_stderr", LevelFilter::WARN);
    if matches!(configurator, Configurator::Apple { .. }) {
        targets = targets.with_target("marathon_core::reporters::timeline", LevelFilter::ERROR);
    }
    targets
}

enum Command {
    Bytes(Vec<u8>),
    Finish,
    Exit(i32),
}

#[derive(Clone)]
pub struct QueueWriter(mpsc::Sender<Command>);
impl Write for QueueWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .send(Command::Bytes(bytes.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console writer stopped"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // Producer flush must not wait for I/O. OutputGuard::shutdown is the exit barrier.
        Ok(())
    }
}

/// Own on the CLI thread, outside the core executor. Shutdown runs after all producers stop.
pub struct OutputGuard {
    writer: QueueWriter,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl OutputGuard {
    pub fn shutdown(mut self) -> io::Result<()> {
        self.finish()
    }
    fn finish(&mut self) -> io::Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.writer.0.send(Command::Finish);
        thread.join().map_err(|_| io::Error::other("console writer panicked"))?
    }
}
impl Drop for OutputGuard {
    fn drop(&mut self) {
        // Fallback for early CLI returns; explicit shutdown reports sink failures.
        let _ = self.finish();
    }
}

type FilterHandle = reload::Handle<Targets, tracing_subscriber::Registry>;

#[derive(Clone)]
pub struct TracingOutput {
    dispatch: Dispatch,
    filter: FilterHandle,
    compact: Arc<AtomicBool>,
    writer: QueueWriter,
}
impl TracingOutput {
    pub fn new(
        sink: impl Write + Send + 'static,
        terminal: bool,
        clock: impl Fn() -> String + Send + Sync + 'static,
    ) -> io::Result<(Self, OutputGuard)> {
        let (send, receive) = mpsc::channel();
        let writer = QueueWriter(send);
        let thread = thread::Builder::new().name("marathon-console".into()).spawn(move || {
            let mut sink = sink;
            let mut failure = None;
            for command in receive {
                match command {
                    Command::Bytes(bytes) => {
                        if let Err(error) = sink.write_all(&bytes).and_then(|()| sink.flush()) {
                            failure.get_or_insert(error);
                        }
                    }
                    Command::Finish => break,
                    Command::Exit(status) => {
                        // process::exit skips guards. Drain and flush here, off core, before exiting.
                        let _ = sink.flush();
                        std::process::exit(status);
                    }
                }
            }
            if let Err(error) = sink.flush() {
                failure.get_or_insert(error);
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
        let output = Self {
            dispatch: Dispatch::new(subscriber),
            filter,
            compact,
            writer: writer.clone(),
        };
        Ok((
            output,
            OutputGuard {
                writer,
                thread: Some(thread),
            },
        ))
    }
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }
    pub fn writer(&self) -> QueueWriter {
        self.writer.clone()
    }
    /// The signal exit hook enqueues its barrier without waiting on the core thread.
    pub fn exit_after_flush(&self, status: i32) -> io::Result<()> {
        self.writer
            .0
            .send(Command::Exit(status))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "console writer stopped"))
    }
    /// Run off core alongside the vendor configurator. No environment logging overrides.
    pub fn configure(&self, configurator: Configurator, debug: bool) -> Result<(), reload::Error> {
        self.filter.reload(filter(configurator, debug))?;
        self.compact.store(
            matches!(configurator, Configurator::Apple { compact: true }),
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
