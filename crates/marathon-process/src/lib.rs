//! Run-owned local processes on Linux and macOS.

mod registry;

use registry::{Registry, Target};
use std::{
    ffi::OsStr,
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::process::{ChildStderr, ChildStdout};

static RUN: Registry = Registry::new();

/// Seal admission and synchronously KILL registered groups without an executor. Returns the count
/// of spawns still in the kernel after the brief wait for late registrations.
pub fn seal_and_kill() -> usize {
    RUN.sweep()
}

pub struct Command(std::process::Command);

impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        let mut command = std::process::Command::new(program);
        command.process_group(0);
        Self(command)
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.0.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.0.envs(vars);
        self
    }

    pub fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.0.current_dir(path);
        self
    }

    pub fn stdin(&mut self, io: Stdio) -> &mut Self {
        self.0.stdin(io);
        self
    }

    pub fn stdout(&mut self, io: Stdio) -> &mut Self {
        self.0.stdout(io);
        self
    }

    pub fn stderr(&mut self, io: Stdio) -> &mut Self {
        self.0.stderr(io);
        self
    }

    pub fn spawn(&mut self) -> io::Result<Child> {
        self.spawn_target(Target::Group)
    }

    /// ADB's existing daemon policy keeps the server even if cancellation races the server's
    /// own session creation. Only the direct client belongs to the run.
    pub fn spawn_adb_client(&mut self) -> io::Result<Child> {
        self.spawn_target(Target::Client)
    }

    fn spawn_target(&mut self, target: impl FnOnce(i32) -> Target) -> io::Result<Child> {
        let ticket = RUN.admit()?;
        let inner = self.0.spawn()?;
        let target = target(inner.id() as i32);
        ticket.register(target);
        drop(ticket);
        Ok(Child {
            registered: true,
            target,
            inner: Some(inner),
        })
    }

    pub async fn output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.spawn()?.wait_with_output().await
    }
}

pub struct Child {
    // Removed before reaping, after which no signal may be sent to the old number.
    registered: bool,
    target: Target,
    inner: Option<std::process::Child>,
}

impl Child {
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.inner.as_mut().and_then(|child| child.stdin.take())
    }

    pub fn take_stdout(&mut self) -> io::Result<Option<ChildStdout>> {
        self.inner
            .as_mut()
            .and_then(|child| child.stdout.take())
            .map(ChildStdout::from_std)
            .transpose()
    }

    pub fn take_stderr(&mut self) -> io::Result<Option<ChildStderr>> {
        self.inner
            .as_mut()
            .and_then(|child| child.stderr.take())
            .map(ChildStderr::from_std)
            .transpose()
    }

    pub fn id(&self) -> Option<u32> {
        self.inner
            .as_ref()
            .filter(|_| self.registered)
            .map(std::process::Child::id)
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn start_kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        if self.registered {
            RUN.signal(self.target, signal)?;
        }
        Ok(())
    }

    pub async fn kill(&mut self) -> io::Result<()> {
        self.start_kill()?;
        self.wait().await?;
        Ok(())
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.finish(false).await
    }

    /// Compatibility for the existing recorder start command. Its old pidfile stop owns the
    /// detached recording until the recorder is migrated to its own handle.
    pub async fn wait_for_legacy_recorder_start(&mut self) -> io::Result<ExitStatus> {
        self.finish(true).await
    }

    async fn finish(&mut self, legacy_recorder_start: bool) -> io::Result<ExitStatus> {
        self.take_stdin();
        let inner = self.inner.as_mut().ok_or_else(|| io::Error::other("missing child"))?;
        if self.registered {
            let mut changed = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
            let clean_exit = loop {
                if let Some(success) = observe_exit(inner.id())? {
                    break success;
                }
                changed.recv().await;
            };
            let keep_recorder = legacy_recorder_start && clean_exit;
            let kill_group = matches!(self.target, Target::Group(_)) && !keep_recorder;
            RUN.retire(self.target, kill_group.then_some(libc::SIGKILL), true)?;
            self.registered = false;
        }
        // WNOWAIT observed the exit while the group was still registered. This wait is nonblocking.
        let status = inner
            .try_wait()?
            .ok_or_else(|| io::Error::other("observed child exit could not be reaped"))?;
        if matches!(self.target, Target::Group(_)) && !(legacy_recorder_start && status.success()) {
            self.confirm_group_exit().await?;
        }
        Ok(status)
    }

    async fn confirm_group_exit(&self) -> io::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // Signal 0 is a read-only check. A reused PGID can only delay confirmation.
                match self.target.signal(0) {
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
                    Err(error) if error.raw_os_error() != Some(libc::EPERM) => return Err(error),
                    _ => tokio::time::sleep(Duration::from_millis(5)).await,
                }
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "process group did not disappear"))?
    }

    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        use tokio::io::{AsyncRead, AsyncReadExt};
        async fn read(pipe: Option<impl AsyncRead + Unpin>) -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_end(&mut bytes).await?;
            }
            Ok(bytes)
        }
        let stdout = self.take_stdout()?;
        let stderr = self.take_stderr()?;
        let (status, stdout, stderr) = tokio::try_join!(self.wait(), read(stdout), read(stderr))?;
        Ok(Output { status, stdout, stderr })
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let Some(mut child) = self.inner.take() else {
            return;
        };
        if self.registered && RUN.retire(self.target, Some(libc::SIGKILL), false).is_err() {
            // The leader is still pinned if the group kill failed.
            let _ = child.kill();
            let _ = RUN.retire(self.target, None, false);
        }
        if !matches!(child.try_wait(), Ok(Some(_))) {
            // kill-on-drop also reaps an abandoned direct child without holding up its caller.
            let _ = std::thread::Builder::new().spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[allow(unsafe_code)]
fn observe_exit(pid: u32) -> io::Result<Option<bool>> {
    loop {
        // SAFETY: waitid writes to the initialized siginfo. WNOWAIT prevents PID reuse.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        return Ok(Some(
            info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0,
        ));
    }
}
