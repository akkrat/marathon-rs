//! Port of core/src/main/kotlin/com/malinskiy/marathon/coroutines/ThreadPoolDispatcher.kt
use crate::{
    error::{Cancellation, Failure},
    runtime::Completion,
};
use jcompat::logging::Level;
#[derive(Clone, Debug)]
pub struct CompletionLog {
    pub level: Level,
    pub message: &'static str,
    pub exception: Option<Failure>,
}
pub const UNRECOVERABLE: &str = "CoroutineContext closing due to unrecoverable error";
pub fn completion_log(completion: Completion) -> CompletionLog {
    match completion {
        Completion::Success => CompletionLog {
            level: Level::Debug,
            message: "CoroutineContext finished",
            exception: None,
        },
        Completion::Cancelled(reason) => CompletionLog {
            level: Level::Debug,
            message: "CoroutineContext cancelled",
            exception: Some(match reason {
                Cancellation::Requested(f) | Cancellation::Parent(f) | Cancellation::Timeout { detail: f, .. } => f,
            }),
        },
        Completion::Failed(f) => CompletionLog {
            level: Level::Error,
            message: UNRECOVERABLE,
            exception: Some(f),
        },
    }
}

impl CompletionLog {
    /// Call on the output executor with the shared registry and rendered trace.
    pub fn render(
        &self,
        registry: &jcompat::logging::MarathonLogging,
        logger: &jcompat::logging::Logger,
        envelope: &jcompat::logging::LogEvent<'_>,
        output: (jcompat::logging::JansiMode, jcompat::logging::OutputTarget),
    ) -> Option<String> {
        registry.enabled(logger, self.level).then(|| {
            registry.layout().format(
                &jcompat::logging::LogEvent {
                    level: self.level,
                    logger: logger.name(),
                    thread: envelope.thread,
                    local_time_millis: envelope.local_time_millis,
                    message: self.message,
                    throwable: envelope.throwable,
                },
                output.0,
                output.1,
            )
        })
    }
}
