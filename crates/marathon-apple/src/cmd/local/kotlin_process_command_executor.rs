//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/local/KotlinProcessCommandExecutor.kt
//! and cmd/local/KotlinProcessCommand.kt.

use super::super::{
    BaseCommand, CommandExecutor, CommandHost, CommandRequest, CommandSession, ExitCode, SessionControl, blocking,
    failure, io_error, millis,
};
use super::KotlinProcessHost;
use crate::extensions::reader::{current_time_millis, produce_lines_manually};
use jcompat::logging::{Logger, MarathonLogging};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use std::{
    os::fd::AsFd,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    process::Command,
    sync::{mpsc, oneshot},
};

// The manual reader polls on its own backoff. Tokio readiness can still be
// Pending after the child exits with bytes in the pipe; that is not Java's
// available()==0 and would let can_read discard those bytes. The child pipes
// are nonblocking, so query the kernel directly without waiting for the reactor.
// Pending registers no waker: only valid under produce_lines_manually, which polls on a timer.
struct PollingPipe<R>(R);
impl<R: AsFd + Unpin> AsyncRead for PollingPipe<R> {
    fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        loop {
            match nix::unistd::read(&self.0, buf.initialize_unfilled()) {
                Ok(count) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(nix::errno::Errno::EAGAIN) => return Poll::Pending,
                Err(error) => return Poll::Ready(Err(std::io::Error::from_raw_os_error(error as i32))),
            }
        }
    }
}

type LogSink = dyn Fn(&Logger, String) + Send + Sync;
pub struct KotlinProcessCommandExecutor {
    runtime: Arc<dyn VendorRuntime>,
    host: CommandHost,
    logger: Logger,
    log_error: Arc<LogSink>,
    destroy_forcibly: bool,
    capacity: usize,
}
impl KotlinProcessCommandExecutor {
    pub fn new(
        runtime: Arc<dyn VendorRuntime>,
        logging: Arc<Mutex<MarathonLogging>>,
        log_error: impl Fn(&Logger, String) + Send + Sync + 'static,
        destroy_forcibly: bool,
        capacity: usize,
    ) -> Self {
        let logger = logging.lock().unwrap_or_else(|e| e.into_inner()).logger(
            "com.malinskiy.marathon.apple.cmd.local.KotlinProcessCommandExecutor",
            None,
        );
        Self {
            runtime,
            host: KotlinProcessHost::host(),
            logger,
            log_error: Arc::new(log_error),
            destroy_forcibly,
            capacity,
        }
    }
}

pub fn strip_quotes(value: &str) -> String {
    fn remove<'a>(value: &'a str, quote: &str) -> &'a str {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            &value[1..value.len() - 1]
        } else {
            value
        }
    }
    remove(remove(value, "\""), "'").to_owned()
}

fn program_with_parent_path(
    program: &str,
    workdir: Option<&str>,
    overrides_path: bool,
) -> VendorResult<std::ffi::OsString> {
    if !overrides_path || program.contains('/') {
        return Ok(program.into());
    }
    // JDK ProcessBuilder searches the parent's PATH, even when environment()
    // supplies a different PATH to the child. Rust Command searches the latter.
    let inherited = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    let cwd = std::env::current_dir().map_err(io_error)?;
    let child_cwd = workdir.map_or(cwd.clone(), |path| cwd.join(path));
    for entry in std::env::split_paths(&inherited) {
        let candidate = child_cwd.join(entry).join(program);
        if candidate.is_file() && nix::unistd::access(&candidate, nix::unistd::AccessFlags::X_OK).is_ok() {
            return Ok(candidate.into_os_string());
        }
    }
    Err(io_error(std::io::Error::from(std::io::ErrorKind::NotFound)))
}

async fn execute(
    runtime: Arc<dyn VendorRuntime>,
    request: CommandRequest,
    context: VendorContext,
    destroy_forcibly: bool,
    capacity: usize,
) -> VendorResult<CommandSession> {
    if let Some(reason) = context.cancellation.reason() {
        return Err(VendorError::Cancelled(reason));
    }
    let mut child = blocking(runtime.as_ref(), move || {
        let command = request.command.iter().map(|arg| strip_quotes(arg)).collect::<Vec<_>>();
        let (program, args) = command
            .split_first()
            .ok_or_else(|| VendorError::Other(failure("java.lang.IndexOutOfBoundsException", "empty command")))?;
        let program = program_with_parent_path(program, request.workdir.as_deref(), request.env.contains_key("PATH"))?;
        let mut builder = Command::new(program);
        builder
            .args(args)
            .envs(&request.env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(path) = &request.workdir {
            builder.current_dir(path);
        }
        builder.spawn().map_err(io_error)
    })
    .await?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalStateException", "missing stdout pipe")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalStateException", "missing stderr pipe")))?;
    let base = BaseCommand::new(capacity, runtime.clone());
    let actual_exit = ExitCode::default();
    let alive = Arc::new(AtomicBool::new(true));
    let (signals, mut signal_rx) = mpsc::unbounded_channel::<(Signal, Option<oneshot::Sender<VendorResult<()>>>)>();
    let actual = actual_exit.clone();
    let alive_monitor = alive.clone();
    // Equivalent to the JVM process reaper: survives total timeout and session-job
    // cancellation, and never owns a deadline or implicitly destroys the child.
    runtime.spawn(Box::pin(async move {
        let result = loop {
            tokio::select! {
                result = child.wait() => break result.map(|status| {
                    use std::os::unix::process::ExitStatusExt;
                    status.code().or_else(|| status.signal().map(|signal| 128 + signal))
                }).map_err(io_error),
                signal = signal_rx.recv(), if !signal_rx.is_closed() || !signal_rx.is_empty() => {
                    if let (Some((signal, ack)), Some(pid)) = (signal, child.id()) {
                        let result = match kill(Pid::from_raw(pid as i32), signal) {
                            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
                            Err(error) => Err(io_error(std::io::Error::from_raw_os_error(error as i32))),
                        };
                        if let Some(ack) = ack { let _ = ack.send(result.clone()); }
                        if let Err(error) = result { break Err(error); }
                    }
                },
            }
        };
        alive_monitor.store(false, Ordering::SeqCst);
        actual.complete(result);
        Ok(())
    }));
    let initial = match current_time_millis(&context) {
        Ok(value) => value,
        Err(error) => {
            let _ = signals.send((Signal::SIGTERM, None));
            return Err(error);
        }
    };
    let last_output = Arc::new(AtomicI64::new(initial));
    let own_context = VendorContext {
        cancellation: base.scope.clone(),
        ..context.clone()
    };
    let exit = base.exit_code.clone();
    let clock = own_context.clock.clone();
    let scope = base.scope.clone();
    let timeout = request.timeout;
    base.scope.track(runtime.spawn(Box::pin(async move {
        let result_exit = exit.clone();
        let timeout_millis = match millis(timeout) {
            Ok(value) => value,
            Err(error) => {
                exit.complete(Err(error));
                return Ok(());
            }
        };
        let result = clock
            .with_timeout_or_null(
                scope.clone(),
                timeout_millis,
                Box::new(move |child| {
                    Box::pin(async move {
                        tokio::select! {
                            biased;
                            reason = child.cancelled() => Err(VendorError::Cancelled(reason)),
                            result = actual_exit.wait() => { result_exit.complete(result); Ok(()) },
                        }
                    })
                }),
            )
            .await;
        match result {
            Ok(None) => {
                exit.complete(Ok(None));
            }
            Ok(Some(())) => {}
            Err(error) => {
                exit.complete(Err(error));
            }
        }
        Ok(())
    })));
    macro_rules! reader {
        ($stream:expr, $output:expr) => {{
            let live = alive.clone();
            let exit = base.exit_code.clone();
            let output = $output.clone();
            let last = last_output.clone();
            let reader_context = own_context.clone();
            let idle_timeout = request.idle_timeout;
            base.scope.track(runtime.spawn(Box::pin(async move {
                produce_lines_manually(
                    PollingPipe($stream),
                    output,
                    last,
                    idle_timeout,
                    reader_context,
                    move || live.load(Ordering::SeqCst) && !exit.is_completed(),
                )
                .await
            })));
        }};
    }
    reader!(stdout, base.stdout);
    reader!(stderr, base.stderr);
    let term = signals.clone();
    let hard = signals.clone();
    let alive_getter = alive.clone();
    Ok(CommandSession::new(
        base,
        SessionControl {
            is_alive: Box::new(move || alive_getter.load(Ordering::SeqCst)),
            terminate: Box::new(move || {
                let _ = term.send((Signal::SIGTERM, None));
                Ok(())
            }),
            kill: Box::new(move || {
                let _ = hard.send((Signal::SIGKILL, None));
                Ok(())
            }),
            close: Box::new(move || {
                let (tx, rx) = oneshot::channel();
                let sent = signals
                    .send((
                        if destroy_forcibly {
                            Signal::SIGKILL
                        } else {
                            Signal::SIGTERM
                        },
                        Some(tx),
                    ))
                    .is_ok();
                Box::pin(async move {
                    // An exited child has closed the signal queue. Otherwise wait
                    // for signal dispatch before cancelling/joining the readers.
                    if sent {
                        match rx.await {
                            Ok(result) => result,
                            Err(_) => Ok(()),
                        }
                    } else {
                        Ok(())
                    }
                })
            }),
        },
    ))
}
impl CommandExecutor for KotlinProcessCommandExecutor {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn logger(&self) -> &Logger {
        &self.logger
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, message: String) {
        (self.log_error)(&self.logger, message);
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession> {
        let runtime = self.runtime.clone();
        let worker = runtime.clone();
        let force = self.destroy_forcibly;
        let capacity = self.capacity;
        Box::pin(async move {
            // This bridge never polls process/pipe plumbing on the core executor.
            let (tx, rx) = oneshot::channel();
            let (accepted, acceptance) = oneshot::channel();
            runtime.spawn(Box::pin(async move {
                match execute(worker, request, context, force, capacity).await {
                    Ok(session) => {
                        // Retain ownership until the caller takes the session, including
                        // cancellation after send succeeds but before receive is polled.
                        let pending = Arc::new(Mutex::new(Some(session)));
                        if tx.send(Ok(pending.clone())).is_ok() {
                            let _ = acceptance.await;
                        }
                        let undelivered = pending.lock().unwrap_or_else(|e| e.into_inner()).take();
                        if let Some(session) = undelivered {
                            session.close().await?;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                    }
                }
                Ok(())
            }));
            let pending = rx
                .await
                .map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", e.to_string())))??;
            let session = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .ok_or_else(|| {
                    VendorError::Other(failure("java.lang.IllegalStateException", "session already delivered"))
                })?;
            // No suspension between taking ownership and returning it to the caller.
            let _ = accepted.send(());
            Ok(session)
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exited_child_pipes_are_drained_before_the_reactor_delivers_readiness() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf 'stdout tail'; printf 'Hello world\\n' >&2"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        assert!(child.wait().unwrap().success());
        let stdout = tokio::process::ChildStdout::from_std(child.stdout.take().unwrap()).unwrap();
        let stderr = tokio::process::ChildStderr::from_std(child.stderr.take().unwrap()).unwrap();
        // No await or runtime turn between registration and these polls: the
        // kernel has bytes, but Tokio has not received the readiness events.
        fn check(mut pipe: impl AsyncRead + Unpin, expected: &[u8]) {
            let mut bytes = [0; 64];
            let mut buf = ReadBuf::new(&mut bytes);
            let mut cx = Context::from_waker(std::task::Waker::noop());
            assert!(matches!(
                Pin::new(&mut pipe).poll_read(&mut cx, &mut buf),
                Poll::Ready(Ok(()))
            ));
            assert_eq!(buf.filled(), expected);
            buf.clear();
            // EOF needs every write end closed. A process that another test in this binary forks at this moment
            // holds a copy until it execs, so the read can be Pending for that long (once in 28 Linux runs).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                match Pin::new(&mut pipe).poll_read(&mut cx, &mut buf) {
                    Poll::Ready(Ok(())) => break,
                    Poll::Pending if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(1))
                    }
                    other => panic!("no EOF: {other:?}"),
                }
            }
            assert!(buf.filled().is_empty());
        }
        check(PollingPipe(stdout), b"stdout tail");
        check(PollingPipe(stderr), b"Hello world\n");
    }
}
