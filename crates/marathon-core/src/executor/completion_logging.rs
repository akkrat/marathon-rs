//! Port of core/src/main/kotlin/com/malinskiy/marathon/coroutines/ThreadPoolDispatcher.kt
#[cfg(any(test, feature = "test-support"))]
use crate::{
    error::{Cancellation, Failure},
    runtime::Completion,
};
#[cfg(any(test, feature = "test-support"))]
use marathon_logging::Level;
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct CompletionLog {
    pub level: Level,
    pub message: &'static str,
    pub exception: Option<Failure>,
}
pub const UNRECOVERABLE: &str = "CoroutineContext closing due to unrecoverable error";
#[cfg(any(test, feature = "test-support"))]
pub fn completion_log(completion: Completion) -> CompletionLog {
    match completion {
        Completion::Success => CompletionLog {
            level: Level::DEBUG,
            message: "CoroutineContext finished",
            exception: None,
        },
        Completion::Cancelled(reason) => CompletionLog {
            level: Level::DEBUG,
            message: "CoroutineContext cancelled",
            exception: Some(match reason {
                Cancellation::Requested(f) | Cancellation::Parent(f) | Cancellation::Timeout { detail: f, .. } => f,
            }),
        },
        Completion::Failed(f) => CompletionLog {
            level: Level::ERROR,
            message: UNRECOVERABLE,
            exception: Some(f),
        },
    }
}
