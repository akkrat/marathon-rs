//! Port of adam 0.5.8 `adam/src/main/kotlin/com/malinskiy/adam/log/AdamLogging.kt`.
//! adam portions: Copyright (C) 2019-2021 Anton Malinskiy; Apache-2.0.
//!
//! adam calls its own logger factory, not marathon's, so its loggers get no level of
//! their own: they inherit the root level. The level check therefore uses
//! [`LogSettings::enabled_target`] on the target alone.

use std::sync::{Arc, Mutex};

use marathon_logging::{Level, LogSettings};

use super::error::AdbError;

/// Receives an enabled event: the logger name, level, message and the attached throwable, if any.
pub type LogSink = dyn Fn(&str, Level, String, Option<&AdbError>) + Send + Sync;

/// The `com.malinskiy.adam.request.Request` logger (`Request.kt:74`).
pub const REQUEST_LOGGER: &str = "marathon_adb::client::log";
/// The `com.malinskiy.adam.AndroidDebugBridgeClient` logger (`AndroidDebugBridgeClient.kt:138`).
pub const CLIENT_LOGGER: &str = "marathon_adb::client::log";

/// The one event path: the enabled check and the delivery, keeping mu's
/// laziness (the message closure runs only if the level is enabled).
type LogFn = dyn Fn(&str, Level, Option<&AdbError>, &mut dyn FnMut() -> String) + Send + Sync;

#[derive(Clone)]
pub struct AdamLogging {
    log: Arc<LogFn>,
}

impl AdamLogging {
    pub fn new(
        logging: Arc<Mutex<LogSettings>>,
        sink: impl Fn(&str, Level, String, Option<&AdbError>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            log: Arc::new(move |logger, level, error, message| {
                let enabled = {
                    let logging = logging.lock().unwrap_or_else(|e| e.into_inner());
                    logging.enabled_target(logger, level)
                };
                if enabled {
                    sink(logger, level, message(), error);
                }
            }),
        }
    }

    /// Explicit dispatch also reaches ADB tasks running outside the CLI thread.
    pub fn traced(dispatch: tracing::Dispatch) -> Self {
        Self {
            log: Arc::new(move |_, level, error, message| {
                tracing::dispatcher::with_default(&dispatch, || {
                    macro_rules! emit {
                        ($macro:ident) => {
                            if let Some(error) = error {
                                tracing::$macro!(error = %error, "{}", message());
                            } else {
                                tracing::$macro!("{}", message());
                            }
                        };
                    }
                    match level {
                        Level::TRACE => emit!(trace),
                        Level::DEBUG => emit!(debug),
                        Level::INFO => emit!(info),
                        Level::WARN => emit!(warn),
                        Level::ERROR => emit!(error),
                    }
                });
            }),
        }
    }

    /// mu's lazy `log.<level> { message }`: the message is built only if the level is enabled.
    pub(crate) fn log(&self, logger: &str, level: Level, error: Option<&AdbError>, message: impl FnOnce() -> String) {
        let mut message = Some(message);
        (self.log)(logger, level, error, &mut move || {
            message.take().map_or_else(String::new, |message| message())
        });
    }
}

impl std::fmt::Debug for AdamLogging {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdamLogging").finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(crate) mod capture {
    //! A sink that records events, for tests.
    use super::*;

    pub type Events = Arc<Mutex<Vec<(String, Level, String)>>>;

    pub fn logging(debug: bool) -> (AdamLogging, Events) {
        let events: Events = Arc::default();
        let mut marathon = LogSettings::default();
        marathon.configure(debug, marathon_logging::Vendor::Android);
        let sink_events = events.clone();
        let logging = AdamLogging::new(Arc::new(Mutex::new(marathon)), move |name, level, message, _| {
            sink_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((name.to_string(), level, message));
        });
        (logging, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_inherited_not_registered() {
        let (logging, events) = capture::logging(true);
        logging.log(REQUEST_LOGGER, Level::DEBUG, None, || "a".into());
        let (quiet, quiet_events) = capture::logging(false);
        // Adam inherits the DEBUG root, which configure does not reset: WARN is built with debug off.
        quiet.log(REQUEST_LOGGER, Level::WARN, None, || "b".into());
        // An ordinary target follows the Android configurator: with debug off its WARN is not built.
        quiet.log("marathon_android::device", Level::WARN, None, || {
            unreachable!("not built when disabled")
        });
        assert_eq!(events.lock().unwrap().len(), 1);
        assert_eq!(
            *quiet_events.lock().unwrap(),
            [(REQUEST_LOGGER.to_owned(), Level::WARN, "b".to_owned())]
        );
    }
}
