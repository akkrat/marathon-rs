//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandSession.kt

use super::channel::ExecChannel;
use super::error::SshFailure;
use crate::cmd::{SessionControl, SessionScope, cancelled};
use russh::Sig;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The backend half of an SSH session; `CommandSession` adds BaseCommand's await/drain and scope close.
pub(crate) fn session_control(
    command: Arc<ExecChannel>,
    scope: Arc<SessionScope>,
    closing: Arc<AtomicBool>,
) -> SessionControl {
    // A11: `override val isAlive = command.isOpen`, evaluated once.
    let alive = command.is_open();
    let closed = Arc::new(AtomicBool::new(false));
    let term = command.clone();
    let kill = command.clone();
    SessionControl {
        is_alive: Box::new(move || alive),
        terminate: Box::new(move || term.signal(Sig::TERM).map_err(SshFailure::into_vendor)),
        kill: Box::new(move || kill.signal(Sig::KILL).map_err(SshFailure::into_vendor)),
        close: Box::new(move || {
            let command = command.clone();
            let closed = closed.clone();
            let closing = closing.clone();
            let scope = scope.clone();
            Box::pin(async move {
                // A14: only the first call does anything, and the flag is set before the work. A later call after
                // a failed close skips this but still cancels and joins the session scope (approved P5).
                if closed.swap(true, Ordering::SeqCst) {
                    return Ok(());
                }
                closing.store(true, Ordering::SeqCst);
                let result = async {
                    if command.is_open() && !command.is_closing() {
                        command.signal(Sig::TERM)?;
                    }
                    // TERM, KILL, CLOSE and at most 30 s in all (D30).
                    command.close_escalating().await
                }
                .await;
                // Nothing may wait on an abandoned session: since `closing` keeps the exit code to the session's
                // cancellation (P2), an awaiter would otherwise wait for a later close that may never come.
                if result.is_err() {
                    scope.cancel(cancelled("Session closed"));
                }
                result.map_err(SshFailure::into_vendor)
            })
        }),
    }
}
