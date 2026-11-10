//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/config/PerformanceDefaultConfig.kt
//!
//! Only the logger factory has an observable effect. The memoised random factories are JVM start-up tuning and
//! are not ported; `KEEP_ALIVE` is `keep_alive::KeepAliveRunner`. Approved P1: of sshj's own lines only those
//! visible at the default ERROR level are reproduced, plus `Will request to exec` under `ssh.debug`.

use super::error::SshFailure;
use marathon_core::error::Failure;
use marathon_logging::{Component, Level, LogSettings};
use std::sync::{Arc, Mutex, Weak};

/// Receives every enabled event; the run's output plumbing formats it (throwable as class + message + causes).
pub type SshLogSink = Arc<dyn Fn(&Component, Level, String, Option<Failure>) + Send + Sync>;

/// The per-run registry plus the sink. The registry lock is never held across an await.
#[derive(Clone)]
pub struct SshLogging {
    pub registry: Arc<Mutex<LogSettings>>,
    pub sink: SshLogSink,
}
impl SshLogging {
    pub fn new(registry: Arc<Mutex<LogSettings>>, sink: SshLogSink) -> Self {
        Self { registry, sink }
    }
    /// `LogSettings.logger {}`: the registry's default level, set again on every lookup (C18).
    #[track_caller]
    pub fn component(&self, name: &str) -> Component {
        self.registry.lock().unwrap_or_else(|e| e.into_inner()).component(name)
    }
    pub fn enabled(&self, logger: &Component, level: Level) -> bool {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled(logger, level)
    }
    pub fn log(
        &self,
        logger: &Component,
        level: Level,
        message: impl FnOnce() -> String,
        throwable: Option<&SshFailure>,
    ) {
        if self.enabled(logger, level) {
            (self.sink)(logger, level, message(), throwable.map(SshFailure::to_failure));
        }
    }
}

/// sshj's `Config` as marathon builds it: every sshj logger is named by the simple class name and set to DEBUG
/// with `ssh.debug`, ERROR otherwise.
#[derive(Clone)]
pub struct PerformanceDefaultConfig {
    pub verbose: bool,
    pub logging: SshLogging,
}
impl PerformanceDefaultConfig {
    pub fn new(verbose: bool, logging: SshLogging) -> Self {
        Self { verbose, logging }
    }
    pub fn logger_for_class(&self, _: Option<&str>) -> Component {
        Component::wire(self.verbose)
    }
    pub fn logger_for_name(&self, _: Option<&str>) -> Component {
        Component::wire(self.verbose)
    }
    /// The loggers one client uses, looked up when it is created, as sshj's constructors do.
    pub fn loggers(&self) -> SshComponents {
        SshComponents {
            transport: self.logger_for_class(Some("marathon_apple::cmd::remote::ssh::sshj::wire")),
            key_exchanger: self.logger_for_class(Some("marathon_apple::cmd::remote::ssh::sshj::wire")),
            promise: self.logger_for_class(Some("marathon_apple::cmd::remote::ssh::sshj::wire")),
            logging: self.logging.clone(),
            config: self.clone(),
        }
    }
}

#[derive(Clone)]
pub struct SshComponents {
    pub transport: Component,
    pub key_exchanger: Component,
    pub promise: Component,
    pub logging: SshLogging,
    pub config: PerformanceDefaultConfig,
}
impl SshComponents {
    /// `TransportImpl.die`: `log.error("Dying because - {}", ex.getMessage(), ex)`.
    pub fn dying(&self, cause: &SshFailure) {
        self.logging.log(
            &self.transport,
            Level::ERROR,
            || format!("Dying because - {}", cause.message().unwrap_or_else(|| "null".into())),
            Some(cause),
        );
    }
    /// `Promise.tryRetrieve`: a waiter woken by a delivered error logs `<<name>> woke to: <toString>`.
    pub fn woke_to(&self, promise: &str, error: &SshFailure) {
        self.logging.log(
            &self.promise,
            Level::ERROR,
            || format!("<<{promise}>> woke to: {error}"),
            None,
        );
    }
}

pub type BridgeSink = dyn Fn(&str, String) + Send + Sync;
static BRIDGE: Mutex<Vec<Weak<BridgeSink>>> = Mutex::new(Vec::new());

struct RusshLog;
impl log::Log for RusshLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("russh") && metadata.level() <= log::Level::Debug
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let sinks: Vec<_> = {
            let mut sinks = BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
            sinks.retain(|sink| sink.strong_count() > 0);
            sinks.iter().filter_map(Weak::upgrade).collect()
        };
        for sink in sinks {
            sink(record.target(), record.args().to_string());
        }
    }
    fn flush(&self) {}
}

/// Under `ssh.debug` russh's own records (trace excluded) reach the run log at DEBUG under their crate targets.
/// The `log` facade is process-wide: the bridge installs itself once, unless another logger already exists,
/// and records go to every registered connection. Nothing is forwarded while no debug client is connected.
pub fn bridge_russh_logs(logging: &SshLogging) -> Arc<BridgeSink> {
    let logging = logging.clone();
    let sink: Arc<BridgeSink> = Arc::new(move |target: &str, message: String| {
        let logger = Component::wire(true);
        let message = format!("{target}: {message}");
        logging.log(&logger, Level::DEBUG, || message, None);
    });
    BRIDGE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::downgrade(&sink));
    if log::set_logger(&RusshLog).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    sink
}
