//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt
//! (shared core/Send producer queue with ordered raw output).
use marathon_logging::Level;
use marathon_core::logging::{LogQueue, LogRecord, OutputRecord};

fn sent(message: &str) -> LogRecord {
    LogRecord { target: module_path!().into(),
        logger: "provider".into(),
        at_millis: 0,
        wall_millis: 0,
        level: Level::WARN,
        message: message.into(),
        cause: None,
    }
}
fn text(record: &OutputRecord) -> String {
    match record {
        OutputRecord::Log(log) => format!("log:{}", log.message.to_owned()),
        OutputRecord::Raw(raw) => format!("raw:{}", raw.to_owned()),
    }
}

#[test]
fn sender_records_drain_in_push_order_with_core_logs_and_raw_text() {
    let q = LogQueue::default();
    let sender = q.sender();
    q.raw("a".into());
    sender.push(sent("b"));
    q.log("core", 1, Level::INFO, "c".into(), None);
    std::thread::spawn(move || sender.push(sent("d"))).join().unwrap();
    q.raw("e".into());
    assert_eq!(
        q.take_output().iter().map(text).collect::<Vec<_>>(),
        ["raw:a", "log:b", "log:c", "log:d", "raw:e"]
    );
    assert!(q.take_output().is_empty());
}

#[test]
fn take_drains_sent_logs_and_leaves_raw_for_the_output_driver() {
    let q = LogQueue::default();
    q.raw("a".into());
    q.sender().push(sent("b"));
    assert_eq!(q.records().len(), 1);
    q.sender().push(sent("c"));
    q.raw("d".into());
    let logs = q.take();
    assert_eq!(
        logs.iter().map(|l| l.message.to_owned()).collect::<Vec<_>>(),
        ["b", "c"]
    );
    q.sender().push(sent("e"));
    assert_eq!(
        q.take_output().iter().map(text).collect::<Vec<_>>(),
        ["raw:a", "raw:d", "log:e"]
    );
}
