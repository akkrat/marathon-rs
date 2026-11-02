//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/logcat/LogcatListener.kt,
//! vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/logcat/LogcatProducer.kt and
//! vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/adam/log/LogcatAccumulatingListener.kt
//!
//! Kotlin `onMessage` is a suspend function; these traits are synchronous
//! (WP 6.5 owns the stream loop that drives them). Registration follows the
//! `marathon-core` `LogProducer` shape: the same `Arc` is retained for
//! removal, because Kotlin removes the listener object by identity.

use crate::logcat::{LogCatMessage, LogLevel};
use jcompat::writers::JavaString;
use std::sync::{Arc, Mutex};

pub trait LogcatListener: Send + Sync {
    fn on_message(&self, message: &LogCatMessage);
    fn close(&self) {}
}

pub trait LogcatProducer: Send + Sync {
    fn add_logcat_listener(&self, listener: Arc<dyn LogcatListener>);
    fn remove_logcat_listener(&self, listener: &Arc<dyn LogcatListener>);
}

/// Collects `AndroidRuntime` ERROR/WARN lines for the "Device log:" warning
/// WP 6.6a logs. Each line keeps the stray trailing `"` of the Kotlin string
/// literal (known Kotlin bug N7, kept under D5).
pub struct LogcatAccumulatingListener {
    producer: Arc<dyn LogcatProducer>,
    buffer: Mutex<JavaString>,
}

impl LogcatAccumulatingListener {
    pub fn new(producer: Arc<dyn LogcatProducer>) -> Arc<Self> {
        Arc::new(Self {
            producer,
            buffer: Mutex::new(JavaString::default()),
        })
    }

    pub fn setup(self: &Arc<Self>) {
        let listener: Arc<dyn LogcatListener> = self.clone();
        self.producer.add_logcat_listener(listener);
    }

    pub fn finish(self: &Arc<Self>) {
        let listener: Arc<dyn LogcatListener> = self.clone();
        self.producer.remove_logcat_listener(&listener);
    }

    pub fn start(&self) {
        self.buffer.lock().unwrap_or_else(|e| e.into_inner()).0.clear();
    }

    /// Returns the accumulated log and clears it; safe to call twice (WP 6.6a
    /// calls it in a `finally` after `finish`).
    pub fn stop(&self) -> JavaString {
        std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl LogcatListener for LogcatAccumulatingListener {
    fn on_message(&self, message: &LogCatMessage) {
        if message.tag().0 == "AndroidRuntime".encode_utf16().collect::<Vec<u16>>() {
            match message.log_level() {
                LogLevel::Error | LogLevel::Warn => {
                    let mut buffer = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
                    buffer.0.push(message.log_level().priority_letter());
                    buffer.push_str("/");
                    buffer.0.extend_from_slice(&message.tag().0);
                    buffer.push_str(": ");
                    buffer.0.extend_from_slice(&message.message.0);
                    buffer.push_str("\"\n");
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logcat::LogCatHeader;

    // New tests derived from the Kotlin source; no upstream tests exist.

    #[derive(Default)]
    struct FakeProducer {
        listeners: Mutex<Vec<Arc<dyn LogcatListener>>>,
    }
    impl FakeProducer {
        fn count(&self) -> usize {
            self.listeners.lock().unwrap_or_else(|e| e.into_inner()).len()
        }
        fn dispatch(&self, message: &LogCatMessage) {
            let listeners = self.listeners.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for listener in listeners {
                listener.on_message(message);
            }
        }
    }
    impl LogcatProducer for FakeProducer {
        fn add_logcat_listener(&self, listener: Arc<dyn LogcatListener>) {
            self.listeners.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
        }
        fn remove_logcat_listener(&self, listener: &Arc<dyn LogcatListener>) {
            self.listeners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|existing| !Arc::ptr_eq(existing, listener));
        }
    }

    fn message(tag: &str, level: LogLevel, text: &str) -> LogCatMessage {
        LogCatMessage::new(
            LogCatHeader {
                log_level: level,
                pid: 1,
                tid: 1,
                app_name: "?".into(),
                tag: tag.into(),
                timestamp: "08-18 16:39:11.760".into(),
            },
            text.into(),
        )
    }

    #[test]
    fn setup_and_finish_register_and_unregister() {
        let producer = Arc::new(FakeProducer::default());
        let listener = LogcatAccumulatingListener::new(producer.clone());
        listener.setup();
        assert_eq!(producer.count(), 1);
        listener.finish();
        assert_eq!(producer.count(), 0);
    }

    #[test]
    fn only_android_runtime_errors_and_warnings_accumulate_with_trailing_quote() {
        let producer = Arc::new(FakeProducer::default());
        let listener = LogcatAccumulatingListener::new(producer.clone());
        listener.setup();
        producer.dispatch(&message("AndroidRuntime", LogLevel::Error, "FATAL EXCEPTION: main"));
        producer.dispatch(&message("AndroidRuntime", LogLevel::Warn, "slow"));
        producer.dispatch(&message("AndroidRuntime", LogLevel::Info, "ignored level"));
        producer.dispatch(&message("TestRunner", LogLevel::Error, "ignored tag"));
        // N7: each accumulated line ends in a stray '"' (known Kotlin bug, kept).
        assert_eq!(
            listener.stop().as_utf8().expect("log"),
            "E/AndroidRuntime: FATAL EXCEPTION: main\"\nW/AndroidRuntime: slow\"\n"
        );
    }

    #[test]
    fn start_clears_and_stop_clears() {
        let producer = Arc::new(FakeProducer::default());
        let listener = LogcatAccumulatingListener::new(producer.clone());
        listener.setup();
        producer.dispatch(&message("AndroidRuntime", LogLevel::Error, "boom"));
        listener.start();
        assert_eq!(listener.stop(), JavaString::default());
        producer.dispatch(&message("AndroidRuntime", LogLevel::Error, "boom"));
        assert!(!listener.stop().0.is_empty());
        // 6.6a calls stop() in a finally after finish(): a second stop is "".
        assert_eq!(listener.stop(), JavaString::default());
    }
}
