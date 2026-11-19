//! Run-owned local processes on Linux and macOS.

pub mod askpass;
mod group;
mod reaper;
mod registry;

use registry::{Registry, Target};
use std::{
    ffi::OsStr,
    io,
    os::{
        fd::AsRawFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{ExitStatus, Output, Stdio},
    time::Duration,
};
use tokio::process::{ChildStderr, ChildStdout};

static RUN: Registry = Registry::new();

/// Install the process-death backstop used by the CLI.
///
/// # Safety
///
/// Call only once, at the start of main, before starting any thread or runtime.
/// The CLI uses Rust's default SIGPIPE disposition.
#[allow(unsafe_code)]
pub unsafe fn start_reaper() -> io::Result<()> {
    // SAFETY: the caller guarantees single-threaded startup.
    RUN.set_reaper(unsafe { reaper::Reaper::start()? });
    Ok(())
}

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
        let mut inner = self.0.spawn()?;
        let target = target(inner.id() as i32);
        let child = Child {
            target,
            stdin: inner.stdin.take(),
            stdout: inner.stdout.take(),
            stderr: inner.stderr.take(),
            inner: Some(inner),
            finished: None,
            confirmed: false,
        };
        ticket.register(target)?;
        Ok(child)
    }

    pub async fn output(&mut self) -> io::Result<Output> {
        self.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        self.spawn()?.wait_with_output().await
    }
}

pub struct Child {
    target: Target,
    // Our handle exists only while the target is registered and its leader is pinned.
    inner: Option<std::process::Child>,
    // Pipe ownership is independent of reaping the leader.
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::process::ChildStdout>,
    stderr: Option<std::process::ChildStderr>,
    finished: Option<ExitStatus>,
    confirmed: bool,
}

impl Child {
    pub fn take_stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> io::Result<Option<ChildStdout>> {
        self.stdout.take().map(ChildStdout::from_std).transpose()
    }

    pub fn take_stderr(&mut self) -> io::Result<Option<ChildStderr>> {
        self.stderr.take().map(ChildStderr::from_std).transpose()
    }

    pub fn id(&self) -> Option<u32> {
        self.inner.as_ref().map(std::process::Child::id)
    }

    pub fn cleanup_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn interrupt(&self) -> io::Result<()> {
        self.signal(libc::SIGINT)
    }

    pub fn start_kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL)
    }

    fn signal(&self, signal: i32) -> io::Result<()> {
        if self.inner.is_none() {
            return Ok(());
        }
        let result = RUN.signal(self.target, signal);
        #[cfg(target_os = "macos")]
        if let (Target::Group(leader), Err(error)) = (self.target, &result)
            && error.raw_os_error() == Some(libc::EPERM)
        {
            // Darwin can refuse a signal when the only remaining members are
            // exiting. Accept dispatch here; wait still owes cleanup proof.
            if matches!(group::all_members_exiting(leader), Ok(true)) {
                return Ok(());
            }
            tracing::warn!(target: "marathon_process", "Could not signal local process group {leader} with signal {signal}: {error}");
        }
        result
    }

    pub async fn kill(&mut self) -> io::Result<()> {
        self.start_kill()?;
        self.wait().await?;
        Ok(())
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.finish_with(confirm_group_exit).await
    }

    async fn finish_with<F, Fut>(&mut self, confirm: F) -> io::Result<ExitStatus>
    where
        F: FnOnce(i32) -> Fut,
        Fut: Future<Output = io::Result<()>>,
    {
        if let Some(status) = self.finished {
            return Ok(status);
        }
        self.take_stdin();
        let pid = self.id().ok_or_else(|| io::Error::other("missing child"))?;
        let mut changed = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
        let status = loop {
            if let Some(status) = observe_exit(pid)? {
                break status;
            }
            changed.recv().await;
        };
        let check_group = matches!(self.target, Target::Group(_));
        if check_group {
            // On macOS EPERM can also mean the group has only zombies. Check
            // for live members with the leader's PID still pinned.
            let kill = RUN.signal(self.target, libc::SIGKILL);
            let check = match kill {
                Err(error) if !(cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EPERM)) => Err(error),
                _ => confirm(pid as i32).await,
            };
            if let Err(error) = check {
                return Ok(self.finish_unconfirmed(status, error, true));
            }
        }
        if let Err(error) = RUN.retire(self.target) {
            // A backed-up pipe must drain before the leader's PID can be released.
            return Ok(self.finish_unconfirmed(status, error, false));
        }
        let mut inner = self.inner.take().ok_or_else(|| io::Error::other("missing child"))?;
        let status = inner
            .try_wait()?
            .ok_or_else(|| io::Error::other("observed child exit could not be reaped"))?;
        self.confirmed = true;
        self.finished = Some(status);
        Ok(status)
    }

    fn finish_unconfirmed(&mut self, status: ExitStatus, error: io::Error, check_group: bool) -> ExitStatus {
        if let Some(child) = self.inner.take() {
            let pid = child.id();
            tracing::warn!(target: "marathon_process", "Could not confirm cleanup of local process {pid}: {error}");
            // WNOWAIT provides the real status. Keep the leader pinned until
            // cleanup can be confirmed and removal reported to the reaper.
            defer_reap(child, self.target, check_group);
        }
        self.finished = Some(status);
        status
    }

    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        use tokio::io::{AsyncRead, AsyncReadExt};
        async fn read(pipe: &mut Option<impl AsyncRead + Unpin>, bytes: &mut Vec<u8>) -> io::Result<()> {
            if let Some(pipe) = pipe {
                pipe.read_to_end(bytes).await?;
            }
            Ok(())
        }
        let (mut stdout_pipe, mut stderr_pipe) = (self.take_stdout()?, self.take_stderr()?);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let status;
        {
            let readers = async {
                tokio::try_join!(read(&mut stdout_pipe, &mut stdout), read(&mut stderr_pipe, &mut stderr))?;
                Ok::<_, io::Error>(())
            };
            tokio::pin!(readers);
            status = tokio::select! {
                biased;
                result = self.wait() => {
                    let status = result?;
                    if self.cleanup_confirmed() {
                        readers.await?;
                    }
                    status
                }
                result = &mut readers => {
                    result?;
                    self.wait().await?
                }
            };
        }
        if !self.cleanup_confirmed() {
            // A descendant may still hold a writer. Collect readable bytes
            // without waiting for EOF or letting continuous output hold exit.
            if let Some(pipe) = &stdout_pipe {
                drain_buffered(pipe, &mut stdout)?;
            }
            if let Some(pipe) = &stderr_pipe {
                drain_buffered(pipe, &mut stderr)?;
            }
        }
        Ok(Output { status, stdout, stderr })
    }
}

#[allow(unsafe_code)]
fn drain_buffered(pipe: &impl AsRawFd, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    // At most 1 MiB per stream, plus the bytes already collected asynchronously.
    for _ in 0..128 {
        // SAFETY: Tokio's ChildStdout and ChildStderr own nonblocking read FDs.
        // The buffer is writable for its full length and the FD outlives the call.
        let len = unsafe { libc::read(pipe.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if len == 0 {
            break;
        }
        if len < 0 {
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => break,
                io::ErrorKind::Interrupted => continue,
                _ => return Err(error),
            }
        }
        bytes.extend_from_slice(&buffer[..len as usize]);
    }
    Ok(())
}

async fn confirm_group_exit(pid: i32) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            #[cfg(target_os = "linux")]
            let remaining = tokio::task::spawn_blocking(move || group::has_other_members(pid))
                .await
                .map_err(io::Error::other)??;
            #[cfg(target_os = "macos")]
            let remaining = group::has_other_members(pid)?;
            if !remaining {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "process group did not disappear"))?
}

fn defer_reap(mut child: std::process::Child, target: Target, check_group: bool) {
    let _ = std::thread::Builder::new().spawn(move || {
        let pid = child.id();
        // std::process::Child::drop never reaps. Even if this thread cannot be
        // started, the target remains pinned for the controller and reaper.
        loop {
            if matches!(observe_exit(pid), Ok(Some(_))) {
                if check_group && !matches!(group::has_other_members(pid as i32), Ok(false)) {
                    let _ = RUN.signal(target, libc::SIGKILL);
                } else if RUN.retire(target).is_ok() {
                    let _ = child.try_wait();
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

impl Drop for Child {
    fn drop(&mut self) {
        let Some(mut child) = self.inner.take() else {
            return;
        };
        let killed = RUN.signal(self.target, libc::SIGKILL);
        if killed.is_err() {
            let _ = child.kill();
        }
        defer_reap(child, self.target, matches!(self.target, Target::Group(_)));
    }
}

#[allow(unsafe_code)]
fn observe_exit(pid: u32) -> io::Result<Option<ExitStatus>> {
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
        // waitid separates the exit code from its cause. ExitStatus expects waitpid's encoding.
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            _ => return Ok(None),
        };
        return Ok(Some(ExitStatus::from_raw(raw)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfirmed_group_keeps_buffered_output() -> io::Result<()> {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf stdout; printf stderr >&2; exit 37"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .finish_with(|_| async { Err(io::Error::other("injected group check failure")) })
            .await?;
        assert!(!child.cleanup_confirmed());
        let output = child.wait_with_output().await?;
        assert_eq!(output.status.code(), Some(37));
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
        Ok(())
    }

    #[tokio::test]
    async fn retirement_is_reported_while_the_leader_is_still_pinned() -> io::Result<()> {
        let mut child = Command::new("/usr/bin/true").spawn()?;
        registry::RETIRE_WITNESS.with(|witness| {
            witness.set(Some((child.target, |pid| {
                assert!(
                    matches!(observe_exit(pid), Ok(Some(_))),
                    "the leader was reaped before retirement"
                );
            })))
        });
        assert!(child.wait().await?.success());
        assert!(registry::RETIRE_WITNESS.with(|witness| witness.get().is_none()));
        Ok(())
    }

    #[tokio::test]
    async fn unconfirmed_group_keeps_the_command_exit_status() -> io::Result<()> {
        for code in [0, 37] {
            let mut child = Command::new("/bin/sh").arg("-c").arg(format!("exit {code}")).spawn()?;
            let status = child
                .finish_with(|_| async {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "injected group confirmation timeout",
                    ))
                })
                .await?;
            assert_eq!(status.code(), Some(code));
            assert!(!child.cleanup_confirmed());
            assert_eq!(child.wait().await?, status);
        }
        Ok(())
    }
}
