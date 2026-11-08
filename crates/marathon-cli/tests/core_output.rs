//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! (shared console adapter regressions).
use jcompat::logging::{JansiMode, Level, MarathonLogging, OutputTarget, Vendor};
use marathon_cli::{application::Console, core_output::RunConsoleSink};
use marathon_core::{
    executor::{CoreExecutor, Signal},
    logging::LogRecord,
    output::OutputDriver,
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};
struct Bytes(Arc<Mutex<Vec<u8>>>);
impl Write for Bytes {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn raw_lines_and_logs_share_startup_console_and_jvm_utf8_encoding() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let console = Arc::new(Console::new(
        Box::new(Bytes(bytes.clone())),
        JansiMode::Default,
        OutputTarget::Pipe,
    ));
    console.write_text("startup\n");
    let registry = Arc::new(Mutex::new(MarathonLogging::default()));
    let sink = Arc::new(RunConsoleSink::new(console.clone(), registry, Box::new(|_| 0)));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw(String::from_utf16_lossy(&vec![0xd800, 10]));
            q.log("progress", 0, Level::Error, "diagnostic".into(), None);
            q.raw("finished\n".into());
            OutputDriver {
                vendor: core.vendor(),
                sink,
            }
            .flush(&q, Signal::new())
            .await
            .map_err(|e| marathon_core::error::OperationError::Failed(e.failure().clone()))?;
            Ok(())
        })
    })
    .unwrap();
    console.write_text("after\n");
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.starts_with("startup\n?\n"));
    assert!(text.find("diagnostic").unwrap() < text.find("finished").unwrap());
    assert!(text.ends_with("finished\nafter\n"));
    assert!(!text.contains('\u{fffd}'));
    assert!(!text.contains('\u{1b}'));
}
struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken"))
    }
}
#[test]
fn stdout_errors_remain_printstream_successes() {
    let console = Arc::new(Console::new(Box::new(Broken), JansiMode::Default, OutputTarget::Pipe));
    let sink = Arc::new(RunConsoleSink::new(
        console,
        Arc::new(Mutex::new(MarathonLogging::default())),
        Box::new(|_| 0),
    ));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw("raw\n".into());
            q.log("progress", 0, Level::Error, "diagnostic".into(), None);
            assert!(
                OutputDriver {
                    vendor: core.vendor(),
                    sink
                }
                .flush(&q, Signal::new())
                .await
                .is_ok()
            );
            Ok(())
        })
    })
    .unwrap();
}
#[test]
fn registered_logger_levels_survive_and_sender_records_keep_raw_order() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let console = Arc::new(Console::new(
        Box::new(Bytes(bytes.clone())),
        JansiMode::Default,
        OutputTarget::Pipe,
    ));
    let mut logging = MarathonLogging::default();
    logging.timings();
    logging.logger("MetricsProviderImpl", None);
    logging.configure(false, Vendor::Android);
    let registry = Arc::new(Mutex::new(logging));
    let sink = Arc::new(RunConsoleSink::new(console, registry.clone(), Box::new(|_| 0)));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw("before\n".into());
            q.sender().push(LogRecord {
                logger: "Timings".into(),
                at_millis: 0,
                wall_millis: 0,
                level: Level::Info,
                message: "timing".into(),
                cause: None,
            });
            q.raw("middle\n".into());
            q.sender().push(LogRecord {
                logger: "MetricsProviderImpl".into(),
                at_millis: 0,
                wall_millis: 0,
                level: Level::Warn,
                message: "provider warning".into(),
                cause: None,
            });
            q.log("progress", 0, Level::Error, "diagnostic".into(), None);
            OutputDriver {
                vendor: core.vendor(),
                sink,
            }
            .flush(&q, Signal::new())
            .await
            .map_err(|e| marathon_core::error::OperationError::Failed(e.failure().clone()))?;
            assert!(q.take_output().is_empty());
            Ok(())
        })
    })
    .unwrap();
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let at = |s: &str| text.find(s).unwrap();
    assert!(at("before") < at("timing") && at("timing") < at("middle") && at("middle") < at("diagnostic"));
    // Registered before configure(false) at DEBUG; re-requesting it would have reset it to ERROR.
    assert!(at("middle") < at("provider warning") && at("provider warning") < at("diagnostic"));
    let registry = registry.lock().unwrap();
    assert!(registry.enabled(&registry.slf4j_logger("Timings"), Level::Info));
}
