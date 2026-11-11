//! WP 8.8 milestone-1 subscriber, CLI, ADB and exit-barrier proof.

use marathon_cli::{
    application::{CliOutput, Process, UnsupportedRunner},
    run_cli,
    tracing_output::{Configurator, TracingOutput},
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex, mpsc},
};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("buffer poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Buffer {
    fn text(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(String::from_utf8(
            self.0.lock().map_err(|_| "buffer poisoned")?.clone(),
        )?)
    }
}
fn setup(buffer: Buffer, terminal: bool) -> io::Result<(TracingOutput, marathon_cli::tracing_output::OutputGuard)> {
    TracingOutput::new(buffer, terminal, |_| "12:34:56.789".into())
}

#[test]
fn cli_version_and_error_drain_before_return() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let process = Process {
        cwd: directory.path().into(),
        environment: vec![],
        now: jcompat::jtime::instant_of(0, 0).ok_or("invalid instant")?,
    };
    for (args, code, expected) in [
        (vec!["version".into()], 0, "Starting marathon"),
        (vec!["run".into()], 1, "No config"),
    ] {
        let buffer = Buffer::default();
        let (tracing, guard) = setup(buffer.clone(), false)?;
        let output = CliOutput::traced(tracing, Box::new(io::sink()));
        assert_eq!(run_cli(&args, &process, &UnsupportedRunner, &output), code);
        guard.shutdown()?;
        let text = buffer.text()?;
        assert!(
            text.starts_with("12:34:56.789 INFO marathon_cli::application: Starting marathon"),
            "{text}"
        );
        assert!(text.contains(expected));
        if code == 0 {
            assert!(text.ends_with(&format!("{}\n", marathon_cli::application::VERSION)));
        }
        assert!(!text.contains('\x1b'));
    }
    Ok(())
}

#[test]
fn configurators_preserve_levels_and_compact_output() -> Result<(), Box<dyn std::error::Error>> {
    for configurator in [
        Configurator::Startup,
        Configurator::Android,
        Configurator::Apple { compact_output: false },
        Configurator::Apple { compact_output: true },
    ] {
        for debug in [false, true] {
            let buffer = Buffer::default();
            let (output, guard) = setup(buffer.clone(), false)?;
            output.configure(configurator, debug)?;
            tracing::dispatcher::with_default(&output.dispatch(), || {
                tracing::info!(target: "marathon_android::device", "ordinary");
                tracing::debug!(target: "marathon_adb::client::log", "adb");
                tracing::info!(target: "marathon_ios::device::timing", "timing");
                tracing::info!(target: "marathon_apple::xcodebuild_stderr", "hidden stderr");
                tracing::warn!(target: "marathon_apple::xcodebuild_stderr", "visible stderr");
                tracing::warn!(target: "marathon_apple::batch_outcome", "visible batch outcome");
                tracing::warn!(target: "marathon_core::reporters::timeline", "timeline");
                tracing::info!(target: "unrelated", "hidden dependency");
                tracing::trace!(target: "marathon_adb::client::log", "hidden trace");
            });
            guard.shutdown()?;
            let text = buffer.text()?;
            assert_eq!(
                text.contains(": ordinary\n"),
                debug || matches!(configurator, Configurator::Startup)
            );
            assert!(text.contains(": adb\n") && text.contains(": timing\n") && text.contains(": visible stderr\n"));
            assert!(text.contains("WARN marathon_apple::batch_outcome: visible batch outcome\n"));
            assert!(!text.contains("hidden"));
            assert_eq!(
                text.contains(": timeline\n"),
                !matches!(configurator, Configurator::Apple { .. })
            );
            assert_eq!(
                text.starts_with("12:34:56.789 "),
                !matches!(configurator, Configurator::Apple { compact_output: true })
            );
        }
    }
    Ok(())
}

#[test]
fn terminal_colour_and_raw_bytes_share_the_ordered_writer() -> Result<(), Box<dyn std::error::Error>> {
    let buffer = Buffer::default();
    let (output, guard) = setup(buffer.clone(), true)?;
    let mut raw = output.writer();
    tracing::dispatcher::with_default(
        &output.dispatch(),
        || tracing::info!(target: "marathon_cli::pilot", "before"),
    );
    let pinned = "100% | [omni]-[device] test.Class#test passed\nMarathon run finished:\n\tRaw: 1 passed, 0 failed, 0 ignored, 0 incomplete tests\n";
    raw.write_all(pinned.as_bytes())?;
    tracing::dispatcher::with_default(
        &output.dispatch(),
        || tracing::warn!(target: "marathon_cli::pilot", "after"),
    );
    guard.shutdown()?;
    let text = buffer.text()?;
    assert!(text.contains("\x1b[32mINFO\x1b[0m"));
    assert!(text.contains(&format!("before\n{pinned}12:34:56.789 \x1b[33mWARN")));
    Ok(())
}

struct Blocked {
    entered: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    buffer: Buffer,
}
impl Write for Blocked {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).map_err(io::Error::other)?;
            self.release.recv().map_err(io::Error::other)?;
        }
        self.buffer.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn a_blocked_sink_never_blocks_or_drops_producers() -> Result<(), Box<dyn std::error::Error>> {
    let buffer = Buffer::default();
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (output, guard) = TracingOutput::new(
        Blocked {
            entered: Some(entered),
            release: blocked,
            buffer: buffer.clone(),
        },
        false,
        |_| "time".into(),
    )?;
    let mut writer = output.writer();
    writer.write_all(b"first\n")?;
    waiting.recv_timeout(std::time::Duration::from_secs(60))?;
    // The sink cannot advance until this same producer releases it. A bounded blocking queue deadlocks here.
    for _ in 0..200_000 {
        writer.write_all(b"queued\n")?;
    }
    release.send(())?;
    guard.shutdown()?;
    assert_eq!(buffer.text()?.lines().count(), 200_001);
    Ok(())
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sink failed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// WP 8.8: once the writer has closed, a late write fails explicitly (and is counted)
/// instead of panicking or silently succeeding; everything written before stays written.
#[test]
fn writes_after_close_are_rejected_not_lost_silently() -> Result<(), Box<dyn std::error::Error>> {
    let buffer = Buffer::default();
    let (output, guard) = setup(buffer.clone(), false)?;
    let mut writer = output.writer();
    writer.write_all(b"before\n")?;
    guard.shutdown()?;
    assert_eq!(buffer.text()?, "before\n");
    let error = writer.write_all(b"too late\n").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(writer.stats().accepted, 1);
    assert_eq!(writer.stats().written, 1);
    assert_eq!(writer.stats().rejected, 1);
    assert_eq!(buffer.text()?, "before\n");
    Ok(())
}

/// WP 8.8 review: queued core and vendor records render their stack traces raw
/// (continuation lines keep their bytes, nothing Debug-quoted on one line) and
/// each line is stamped from the record's own `wall_millis`, not drain time.
#[test]
fn queued_records_render_raw_traces_at_their_own_time() -> Result<(), Box<dyn std::error::Error>> {
    use marathon_core::logging::LogRecord;
    let buffer = Buffer::default();
    let (output, guard) = TracingOutput::new(buffer.clone(), false, |wall: Option<i64>| {
        marathon_cli::application::day_stamp(wall.unwrap_or(0))
    })?;
    let mut failure =
        marathon_core::error::Failure::from_kind(marathon_error::FailureKind::Runtime, Some("boom: x".into()));
    failure.stacktrace = Some("\tat first\n\tat second\n".into());
    let trace = failure.stack_trace();
    // A core record and a vendor-shaped record (logger carries the device identity).
    let core_record = LogRecord {
        target: "marathon_core::logging".into(),
        logger: "marathon_core::marathon".into(),
        at_millis: 0,
        wall_millis: 45_296_789,
        level: marathon_logging::Level::ERROR,
        message: "run failed".into(),
        cause: Some(failure.clone()),
    };
    let vendor_record = LogRecord {
        target: "marathon_core::execution::device::actor".into(),
        logger: "DevicePool[omni]_DeviceActor[emulator-5554]".into(),
        at_millis: 0,
        wall_millis: 45_296_790,
        level: marathon_logging::Level::ERROR,
        message: "device failed".into(),
        cause: Some(failure),
    };
    tracing::dispatcher::with_default(&output.dispatch(), || {
        // A macro event has no record time: the stamp asks for now (0 here).
        tracing::info!(target: "marathon_cli::application", "macro event");
        core_record.emit();
        vendor_record.emit();
    });
    guard.shutdown()?;
    let text = buffer.text()?;
    assert!(
        text.contains("00:00:00.000 INFO marathon_cli::application: macro event\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "12:34:56.789 ERROR marathon_core::marathon: run failed error={trace}"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "12:34:56.790 ERROR marathon_core::execution::device::actor: device failed error={trace} \
             component=DevicePool[omni]_DeviceActor[emulator-5554]"
        )),
        "{text}"
    );
    assert!(!text.contains("\\n"), "escaped trace in {text:?}");
    assert!(!text.contains("error=\""), "quoted trace in {text:?}");
    Ok(())
}

#[test]
fn shutdown_reports_sink_failure() -> io::Result<()> {
    let (output, guard) = TracingOutput::new(Broken, false, |_| "time".into())?;
    output.writer().write_all(b"line\n")?;
    assert_eq!(
        guard.shutdown().err().map(|e| e.to_string()).as_deref(),
        Some("sink failed")
    );
    Ok(())
}

#[test]
fn cancellation_flushes_even_when_process_exit_skips_guards() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "MARATHON_TRACING_EXIT_PROBE";
    if let Some(mode) = std::env::var_os(CHILD) {
        let sink: Box<dyn Write + Send> = if mode == "broken" {
            Box::new(Broken)
        } else {
            Box::new(io::stdout())
        };
        let (output, guard) = TracingOutput::new(sink, false, |_| "12:34:56.789".into())?;
        if mode == "closed" {
            // The writer has already finished: the Exit command has nowhere to
            // go, its acknowledgement disconnects, and the watchdog exits at once.
            guard.shutdown()?;
            output.exit_after_flush(130)?;
            loop {
                std::thread::park();
            }
        }
        tracing::dispatcher::with_default(
            &output.dispatch(),
            || tracing::warn!(target: "marathon_cli::pilot", "cancelled"),
        );
        output
            .writer()
            .write_all(b"Marathon run finished:\n\tRaw: 0 passed, 0 failed, 0 ignored, 1 incomplete tests\n")?;
        output.exit_after_flush(130)?;
        loop {
            std::thread::park();
        }
    }
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "cancellation_flushes_even_when_process_exit_skips_guards",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()?;
    assert_eq!(child.status.code(), Some(130));
    let text = String::from_utf8(child.stdout)?;
    assert!(text.contains("WARN marathon_cli::pilot: cancelled\nMarathon run finished:\n\tRaw: 0 passed, 0 failed, 0 ignored, 1 incomplete tests\n"), "{text}");
    let broken = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "cancellation_flushes_even_when_process_exit_skips_guards",
            "--nocapture",
        ])
        .env(CHILD, "broken")
        .output()?;
    assert_eq!(broken.status.code(), Some(130));
    let closed = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "cancellation_flushes_even_when_process_exit_skips_guards",
            "--nocapture",
        ])
        .env(CHILD, "closed")
        .output()?;
    assert_eq!(closed.status.code(), Some(130));
    Ok(())
}

#[tokio::test]
async fn adb_rejection_reaches_the_same_subscriber_from_another_thread() -> Result<(), Box<dyn std::error::Error>> {
    use marathon_adb::client::{AdamLogging, AndroidDebugBridgeClientFactory, misc::GetAdbServerVersionRequest};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut request = [0; 16];
        socket.read_exact(&mut request).await?;
        assert_eq!(&request, b"000Chost:version");
        socket.write_all(b"FAIL0004nope").await?;
        io::Result::Ok(())
    });
    let buffer = Buffer::default();
    let (output, guard) = setup(buffer.clone(), false)?;
    let client = AndroidDebugBridgeClientFactory {
        host: Some(address.ip()),
        port: Some(address.port()),
        logging: Some(AdamLogging::traced(output.dispatch())),
        ..Default::default()
    }
    .build();
    let worker = std::thread::spawn(move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(client.execute(GetAdbServerVersionRequest, None));
        assert!(matches!(
            result,
            Err(marathon_adb::client::AdbError::RequestRejected(_))
        ));
        Ok(())
    });
    tokio::task::spawn_blocking(move || {
        worker
            .join()
            .map_err(|_| "ADB thread panicked")?
            .map_err(|e| e.to_string())
    })
    .await?
    .map_err(io::Error::other)?;
    server.await??;
    guard.shutdown()?;
    assert_eq!(
        buffer.text()?,
        "12:34:56.789 WARN marathon_adb::client::log: adb server rejected command 000Chost:version\n"
    );
    Ok(())
}

#[test]
fn crash_events_preserve_external_trace_whitespace() -> Result<(), Box<dyn std::error::Error>> {
    use marathon_cli::application::{CliRunner, Console, MarathonRun};
    use marathon_core::{error::Failure, model::ExecutionCommand};
    use marathon_logging::LogSettings;

    struct Crash;
    impl CliRunner for Crash {
        fn construct(
            &self,
            _: Arc<marathon_config::Configuration>,
            _: Arc<Mutex<LogSettings>>,
            _: Arc<Console>,
        ) -> Result<Box<dyn MarathonRun>, Failure> {
            Ok(Box::new(Crash))
        }
    }
    impl MarathonRun for Crash {
        fn run(self: Box<Self>, _: ExecutionCommand) -> Result<bool, Failure> {
            let mut failure = Failure::from_kind(marathon_error::FailureKind::Runtime, Some("failed".into()));
            failure.stacktrace = Some("external frame \t\n".into());
            Err(failure)
        }
    }
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("Marathonfile"),
        "name: pilot\noutputDir: out\nuncompletedTestRetryQuota: 5\nvendorConfiguration: {type: Android, applicationApk: app.apk}\n",
    )?;
    let process = Process {
        cwd: directory.path().into(),
        environment: vec![("ANDROID_HOME".into(), "/android-sdk".into())],
        now: jcompat::jtime::instant_of(0, 0).ok_or("invalid instant")?,
    };
    let buffer = Buffer::default();
    let (tracing, guard) = setup(buffer.clone(), false)?;
    let output = CliOutput::traced(tracing, Box::new(io::sink()));
    assert_eq!(run_cli(&["run".into()], &process, &Crash, &output), 1);
    guard.shutdown()?;
    let text = buffer.text()?;
    assert!(text.contains("external frame \t\n"), "{text:?}");
    Ok(())
}

#[test]
fn sigquit_installation_failure_warns_once_and_remains_visible_with_debug_off() -> Result<(), Box<dyn std::error::Error>>
{
    let buffer = Buffer::default();
    let (tracing, guard) = setup(buffer.clone(), false)?;
    tracing.configure(Configurator::Android, false)?;
    let output = CliOutput::traced(tracing, Box::new(io::sink()));
    let error = io::Error::other("injected SIGQUIT registration failure");
    for _ in 0..2 {
        marathon_cli::runner::warn_quit_installation(&error, &output.console());
    }
    guard.shutdown()?;
    let text = buffer.text()?;
    assert_eq!(
        text.matches("WARN marathon_core::diagnostics: SIGQUIT state dumps unavailable:")
            .count(),
        1,
        "{text}"
    );
    assert!(text.contains("continuing without SIGQUIT diagnostics"), "{text}");
    Ok(())
}

#[test]
fn cancellation_diagnostics_keep_stored_failure_data_x56() -> Result<(), Box<dyn std::error::Error>> {
    use marathon_core::{
        error::{Failure, FailureKind},
        logging::LogRecord,
    };
    let cause = Failure::from_kind(
        FailureKind::JobCancelled,
        Some("LazyDeferredCoroutine was cancelled".into()),
    );
    let mut failure = Failure::from_kind(
        FailureKind::DeviceLost,
        Some(format!("{cause}; job=LazyDeferredCoroutine{{Cancelled}}")),
    );
    failure.cause = Some(Box::new(cause));
    let record = LogRecord {
        target: "marathon_core::execution::device::actor".into(),
        logger: "marathon_core::execution::device::actor".into(),
        at_millis: 0,
        wall_millis: 0,
        level: marathon_logging::Level::ERROR,
        message: "Critical error during execution".into(),
        cause: Some(failure.clone()),
    };
    let buffer = Buffer::default();
    let (output, guard) = setup(buffer.clone(), false)?;
    tracing::dispatcher::with_default(&output.dispatch(), || record.emit());
    guard.shutdown()?;
    let text = buffer.text()?;
    assert!(
        text.contains("error=device lost: job cancelled: task was cancelled"),
        "{text}"
    );
    assert!(text.contains("Caused by: job cancelled: task was cancelled"));
    assert!(!text.contains("Coroutine"));
    assert_eq!(record.cause, Some(failure));
    assert!(
        record
            .cause
            .as_ref()
            .ok_or("cause")?
            .stack_trace()
            .contains("LazyDeferredCoroutine")
    );
    Ok(())
}
