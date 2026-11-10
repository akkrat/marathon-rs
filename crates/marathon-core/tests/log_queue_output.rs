//! Port of core/src/main/kotlin/com/malinskiy/marathon/log/MarathonLogging.kt
//! (shared core/Send producer queue with ordered raw output).
use marathon_core::logging::{LogQueue, LogRecord, OutputRecord};
use marathon_logging::Level;
use std::sync::{Arc, Mutex};

fn sent(message: &str) -> LogRecord {
    LogRecord {
        target: module_path!().into(),
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

/// WP 8.8: vendor records pushed while final shutdown is already forwarding the queue are
/// not lost and do not panic: everything pushed before a close fence is sunk in order, and a
/// record pushed after the fence is forwarded by the same task (a second fence proves it).
#[test]
fn late_output_forwards_records_pushed_before_and_after_the_close_fence() {
    let q = LogQueue::default();
    let late = q.late_output();
    let sender = q.sender();
    q.raw("before".into());
    sender.push(sent("queued"));
    let seen = Arc::new(Mutex::new(vec![]));
    let sink = {
        let seen = seen.clone();
        Arc::new(move |record: OutputRecord| seen.lock().unwrap().push(text(&record)))
    };
    late.close(sink.clone()).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["raw:before", "log:queued"]);
    sender.push(sent("late"));
    late.close(sink).unwrap();
    assert_eq!(*seen.lock().unwrap(), ["raw:before", "log:queued", "log:late"]);
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
