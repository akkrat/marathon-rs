//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/progress/PoolProgressAccumulator.kt
//! (shared console adapter regressions).
use marathon_cli::{application::Console, core_output::RunConsoleSink};
use marathon_core::{
    executor::{CoreExecutor, Signal},
    logging::LogRecord,
    output::OutputDriver,
};
use marathon_logging::{Level, LogSettings, OutputTarget, Vendor};
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
fn raw_lines_and_logs_share_startup_console_and_native_utf8_encoding() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (console, guard) = Console::new(Box::new(Bytes(bytes.clone())), OutputTarget::Pipe)?;
    let console = Arc::new(console);
    console.write_text("startup\n");
    let registry = Arc::new(Mutex::new(LogSettings::default()));
    let sink = Arc::new(RunConsoleSink::new(console.clone(), registry, Box::new(|_| 0)));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw(String::from_utf16_lossy(&[0xd800, 10]));
            q.log("progress", 0, Level::ERROR, "diagnostic".into(), None);
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
    guard.shutdown_with_timeout(std::time::Duration::from_secs(60))?;
    let text = String::from_utf8(bytes.lock().unwrap().clone())?;
    assert!(text.starts_with("startup\n�\n"));
    assert!(text.find("diagnostic").unwrap() < text.find("finished").unwrap());
    assert!(text.ends_with("finished\nafter\n"));
    assert_eq!(text.matches('\u{fffd}').count(), 1);
    assert!(!text.contains('\u{1b}'));
    Ok(())
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
fn stdout_errors_remain_printstream_successes() -> Result<(), Box<dyn std::error::Error>> {
    let (console, guard) = Console::new(Box::new(Broken), OutputTarget::Pipe)?;
    let console = Arc::new(console);
    let sink = Arc::new(RunConsoleSink::new(
        console,
        Arc::new(Mutex::new(LogSettings::default())),
        Box::new(|_| 0),
    ));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw("raw\n".into());
            q.log("progress", 0, Level::ERROR, "diagnostic".into(), None);
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
    assert!(guard.shutdown().is_err());
    Ok(())
}
#[test]
fn registered_logger_levels_survive_and_sender_records_keep_raw_order() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (console, guard) = Console::new(Box::new(Bytes(bytes.clone())), OutputTarget::Pipe)?;
    let console = Arc::new(console);
    let mut logging = LogSettings::default();
    logging.timings();
    logging.component("marathon_core::analytics::provider");
    logging.configure(false, Vendor::Android);
    let registry = Arc::new(Mutex::new(logging));
    let sink = Arc::new(RunConsoleSink::new(console, registry.clone(), Box::new(|_| 0)));
    CoreExecutor::run(move |core| {
        Box::pin(async move {
            let q = core.log_queue();
            q.raw("before\n".into());
            q.sender().push(LogRecord {
                target: module_path!().into(),
                logger: "marathon_core::timing".into(),
                at_millis: 0,
                wall_millis: 0,
                level: Level::INFO,
                message: "timing".into(),
                cause: None,
            });
            q.raw("middle\n".into());
            q.sender().push(LogRecord {
                target: module_path!().into(),
                logger: "marathon_core::analytics::provider".into(),
                at_millis: 0,
                wall_millis: 0,
                level: Level::WARN,
                message: "provider warning".into(),
                cause: None,
            });
            q.log("progress", 0, Level::ERROR, "diagnostic".into(), None);
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
    guard.shutdown_with_timeout(std::time::Duration::from_secs(60))?;
    let text = String::from_utf8(bytes.lock().unwrap().clone())?;
    let at = |s: &str| text.find(s).unwrap();
    assert!(at("before") < at("timing") && at("timing") < at("middle") && at("middle") < at("diagnostic"));
    // Registered before configure(false) at DEBUG; re-requesting it would have reset it to ERROR.
    assert!(at("middle") < at("provider warning") && at("provider warning") < at("diagnostic"));
    let registry = registry.lock().unwrap();
    assert!(registry.enabled(&registry.component("marathon_core::timing"), Level::INFO));
    Ok(())
}
