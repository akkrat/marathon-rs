//! D93: whether a command that failed to open is known to have started nothing on the worker; Rust-only lifecycle support
//! for the ports of XcodebuildSession.kt and the SSH executor. An xcodebuild session whose open failed counts as closed
//! only when nothing can be running: the channel was refused or never opened, or the exec request was refused, so no
//! exec reached a shell. Any other failure (an exec sent whose reply never came, a transport that died after it) leaves
//! the outcome unknown, and the device's stop unconfirmed.
//!
//! The answer travels in a task-local scope set around the opener, because the command's error passes through layers
//! (binaries, the device's command handle) that carry only its `VendorError`. An executor reports the outcome of each
//! execute it ends in the caller's task; one that does not report leaves the outcome unknown, so a lost report is safe.

use std::{cell::Cell, future::Future};

tokio::task_local! {
    static STARTED_NOTHING: Cell<Option<bool>>;
}

/// The latest execute in this scope ended: `started_nothing` if it failed knowing that nothing reached the worker.
pub fn record(started_nothing: bool) {
    let _ = STARTED_NOTHING.try_with(|cell| cell.set(Some(started_nothing)));
}

/// Runs `body`, and says whether the last execute in it failed having started nothing.
pub async fn observe<T>(body: impl Future<Output = T>) -> (T, bool) {
    let (output, reported) = reported(body).await;
    (output, reported.unwrap_or(false))
}

/// Runs `body`, and returns what its last execute reported, if any execute in it did. A layer over an executor (the
/// device's command handle) passes the report on with [`record`] and adds its own only where the executor gave none.
pub async fn reported<T>(body: impl Future<Output = T>) -> (T, Option<bool>) {
    STARTED_NOTHING
        .scope(Cell::new(None), async move {
            let output = body.await;
            (output, STARTED_NOTHING.with(Cell::get))
        })
        .await
}
