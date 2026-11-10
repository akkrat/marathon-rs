//! Native run diagnostics for the SIGQUIT equivalent of the JVM thread dump.

use std::fmt::Write;

pub const TARGET: &str = "marathon_core::diagnostics";
pub(crate) const MAX_POOLS: usize = 16;
pub(crate) const MAX_DEVICES: usize = 32;
pub(crate) const MAX_TESTS: usize = 8;
pub(crate) const MAX_DUMP_BYTES: usize = 64 * 1024;

/// Keep identifiers on one line and bound bytes even for user-controlled names.
pub(crate) fn name(value: &str) -> String {
    let mut result = String::new();
    for c in value.chars() {
        if result.len() + c.len_utf8() > 160 {
            result.push('…');
            break;
        }
        result.push(if c.is_control() { '?' } else { c });
    }
    result
}

pub(crate) fn bound(out: &mut String) {
    const TAIL: &str = "\n  ... dump byte limit reached\n";
    if out.len() > MAX_DUMP_BYTES {
        let mut end = MAX_DUMP_BYTES - TAIL.len();
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str(TAIL);
    }
}

pub(crate) fn job(job: &crate::runtime::JobRef) -> &'static str {
    use crate::runtime::JobState;
    match job.state() {
        JobState::Active => "active",
        JobState::Completing => "completing",
        JobState::Cancelling(_) => "cancelling",
        JobState::Completed(_) => "completed",
    }
}

#[cfg(unix)]
pub(crate) fn header() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("SIGQUIT state dump at unix_ms={millis}\n")
}

pub(crate) fn omitted(out: &mut String, count: usize, kind: &str) {
    if count > 0 {
        let _ = writeln!(out, "  ... {count} {kind} omitted");
    }
}

/// A broken diagnostic must not fail its owning core job and cancel execution.
#[cfg(unix)]
pub(crate) fn capture(build: impl FnOnce() -> String) -> String {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(build)) {
        Ok(dump) => dump,
        Err(_) => "SIGQUIT state dump unavailable: construction panicked; run continues".into(),
    }
}

#[cfg(unix)]
static INSTALLATION: std::sync::OnceLock<Result<(), (std::io::ErrorKind, String)>> = std::sync::OnceLock::new();

#[cfg(unix)]
static REQUESTS: std::sync::OnceLock<QuitRequests> = std::sync::OnceLock::new();

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct QuitRequests {
    pending: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}
#[cfg(unix)]
impl QuitRequests {
    fn signal(&self) {
        // Notify alone stores one permit and would coalesce deliveries *after*
        // the OS stream had observed them. Keep each observed delivery instead.
        let _ = self.pending.try_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |n| Some(n.saturating_add(1)),
        );
        self.changed.notify_one();
    }
    fn take(&self) -> bool {
        self.pending
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_sub(1),
            )
            .is_ok()
    }
    pub(crate) fn discard(&self) {
        self.pending.store(0, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) async fn next(&self) {
        loop {
            let changed = self.changed.notified();
            if self.take() {
                return;
            }
            changed.await;
        }
    }
}

/// Install before config I/O. The signal driver lives until process exit, independently
/// of D52 cancellation and of the run's vendor runtime. Before/after a run no dump is needed.
#[cfg(unix)]
pub fn install_quit_handler() -> std::io::Result<()> {
    // Remember both failure and inherited ignore: main and embedded runners share
    // one attempt, and a later disposition change must not override initial SIG_IGN.
    let result = INSTALLATION.get_or_init(|| {
        if crate::marathon::signal_ignored(nix::libc::SIGQUIT) {
            return Ok(());
        }
        install().map_err(|error| (error.kind(), error.to_string()))
    });
    match result {
        Ok(()) => Ok(()),
        Err((kind, message)) => Err(std::io::Error::new(*kind, message.clone())),
    }
}

#[cfg(unix)]
fn install() -> std::io::Result<()> {
    let requests = REQUESTS.get_or_init(QuitRequests::default);
    let (ready, installed) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("marathon-diagnostics".into())
        .spawn(move || {
            // Build/register on the driver's own thread so failed setup can also
            // drop its runtime safely when an embedded caller is already async.
            let setup = (|| -> std::io::Result<_> {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build()?;
                let stream = {
                    let _entered = runtime.enter();
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit())?
                };
                Ok((runtime, stream))
            })();
            match setup {
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
                Ok((runtime, mut stream)) => {
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    runtime.block_on(async move {
                        while stream.recv().await.is_some() {
                            requests.signal();
                        }
                    });
                }
            }
        })?;
    installed
        .recv()
        .map_err(|_| std::io::Error::other("SIGQUIT driver stopped during installation"))?
}

#[cfg(unix)]
pub(crate) fn requests() -> Option<&'static QuitRequests> {
    INSTALLATION.get()?.as_ref().ok()?;
    REQUESTS.get()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn a_panicking_dump_does_not_end_the_listener_task() {
        let listener = tokio::spawn(async {
            let state = std::cell::RefCell::new(0);
            let warning = super::capture(|| {
                let _borrow = state.borrow_mut();
                panic!("injected snapshot panic");
            });
            let next = super::capture(|| {
                *state.borrow_mut() += 1;
                "next SIGQUIT dump".into()
            });
            (warning, next)
        });
        let result = listener.await;
        assert!(result.is_ok(), "dump panic escaped the listener: {result:?}");
        let (warning, next) = result.unwrap();
        assert!(warning.contains("construction panicked; run continues"));
        assert_eq!(next, "next SIGQUIT dump");
    }

    #[cfg(unix)]
    #[test]
    fn observed_signals_are_retained_until_the_core_takes_each_one() {
        let requests = super::QuitRequests::default();
        requests.signal();
        requests.signal();
        assert!(requests.take());
        assert!(requests.take());
        assert!(!requests.take());
    }

    #[test]
    fn dump_byte_limit_preserves_utf8() {
        let mut dump = "é".repeat(super::MAX_DUMP_BYTES);
        super::bound(&mut dump);
        assert!(dump.len() <= super::MAX_DUMP_BYTES);
        assert!(dump.ends_with("dump byte limit reached\n"));
    }
    #[test]
    fn identifiers_are_bounded_and_cannot_add_lines() {
        let text = super::name(&"é\n".repeat(1000));
        assert!(text.len() <= 163);
        assert!(!text.contains('\n'));
        assert!(text.ends_with('…'));
    }
}
