//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt

use super::RsyncTarget;
use crate::cmd::{
    FileBridge, SessionScope, blocking, failure, io_error,
    remote::ssh::sshj::{client::within, config::SshLogging},
    retained_tasks::RetainedTasks,
};
use jcompat::{
    file_manager::absolute_file_path,
    logging::{Level, Logger},
};
use marathon_config::vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorRuntime},
};
use std::{
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Notify, oneshot},
};

/// D31: a send fails after this long with no I/O. A limit on progress, not on the total, because a large bundle over a
/// slow link is legitimate. Long, because a worker can be silent for a while with the disk under load, as when it is
/// booting simulators, and failing a healthy send costs more than noticing a dead one later.
pub const SEND_IO_TIMEOUT_SECONDS: i64 = 300;
/// D31: what rsync prints when `--timeout` expires: openrsync's `error: poll: timeout`, rsync's `io timeout after N seconds`.
const STALL_REPORTS: [&[u8]; 2] = [b"poll: timeout", b"io timeout after "];
/// D31: how long a process that reported a stall gets to end at each step. It only has to outlast one on its way out.
const STOP_STEP_MILLIS: i64 = 5_000;
const STOP_POLL_MILLIS: i64 = 100;
/// D31: real time, not the vendor clock: this bounds how soon other processes get scheduled (`pgrep`, a pipe's last bytes),
/// which is no shorter in a test that compresses the clock.
const PROCESS_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub struct RsyncFileBridge(Arc<Inner>);
struct Inner {
    target: RsyncTarget,
    debug: bool,
    ssh: SshConfiguration,
    config: RsyncConfiguration,
    auth: Option<SshAuthentication>,
    runtime: Arc<dyn VendorRuntime>,
    clock: Arc<dyn VendorClock>,
    logging: SshLogging,
    logger: Logger,
    send: tokio::sync::Mutex<()>,
    tasks: RetainedTasks,
}
impl RsyncFileBridge {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        target: RsyncTarget,
        debug: bool,
        ssh: SshConfiguration,
        config: RsyncConfiguration,
        auth: Option<SshAuthentication>,
        runtime: Arc<dyn VendorRuntime>,
        clock: Arc<dyn VendorClock>,
        logging: SshLogging,
    ) -> VendorResult<Self> {
        let logger = logging.logger("com.malinskiy.marathon.apple.cmd.remote.rsync.RsyncFileBridge");
        let bridge = Self(Arc::new(Inner {
            target,
            debug,
            ssh,
            config,
            auth,
            runtime,
            clock,
            logging,
            logger,
            send: tokio::sync::Mutex::new(()),
            tasks: RetainedTasks::default(),
        }));
        if debug {
            let inner = bridge.0.clone();
            bridge
                .owned(async move {
                    let output = inner
                        .process(
                            vec![rsync_program(), "--version".into(), "/tmp".into(), "/tmp".into()],
                            false,
                        )
                        .await?;
                    let first = output.1.split('\n').next().unwrap_or_default().to_owned();
                    inner.logging.log(&inner.logger, Level::Debug, || first, None);
                    Ok(true)
                })
                .await?;
        }
        Ok(bridge)
    }
    async fn owned(&self, body: impl Future<Output = VendorResult<bool>> + Send + 'static) -> VendorResult<bool> {
        let (tx, rx) = oneshot::channel();
        let task = self.0.tasks.spawn(self.0.runtime.as_ref(), async move {
            let _ = tx.send(body.await);
            Ok(())
        });
        task.join().await?;
        rx.await
            .map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", e.to_string())))?
    }
    /// Joins work retained after an awaiter was dropped. No process cleanup is hidden in Drop.
    pub async fn join_pending(&self) -> VendorResult<()> {
        let mut result = Ok(());
        loop {
            let tasks = self.0.tasks.take();
            if tasks.is_empty() {
                return result;
            }
            for task in tasks {
                result = result.and(task.join().await);
            }
        }
    }
    #[doc(hidden)]
    pub fn retained_tasks(&self) -> usize {
        self.0.tasks.len()
    }
    /// Exact host argv, useful to consumers recording transport evidence.
    pub fn arguments(&self, source: String, destination: String, receive: bool) -> VendorResult<Vec<String>> {
        self.0.arguments(source, destination, receive)
    }
}
#[derive(Clone)]
struct BinaryPaths {
    rsync: String,
    sshpass: String,
}
// rsync4j's Binaries caches discovery per process, independently of bridge instances.
static BINARIES: Mutex<Option<BinaryPaths>> = Mutex::new(None);
fn executable(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.into())
}
fn rsync_program() -> String {
    BINARIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.rsync.clone())
        .unwrap_or_else(|| executable("RSYNC4J_RSYNC", "/usr/bin/rsync"))
}
fn sshpass_program() -> String {
    BINARIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.sshpass.clone())
        .unwrap_or_else(|| executable("RSYNC4J_SSHPASS", "/usr/bin/sshpass"))
}
fn initialize_binaries() -> VendorResult<()> {
    let mut paths = BINARIES.lock().unwrap_or_else(|e| e.into_inner());
    if paths.is_some() {
        return Ok(());
    }
    let rsync = executable("RSYNC4J_RSYNC", "/usr/bin/rsync");
    let sshpass = executable("RSYNC4J_SSHPASS", "/usr/bin/sshpass");
    for (name, variable, path) in [
        ("rsync", "RSYNC4J_RSYNC", rsync.clone()),
        ("ssh", "RSYNC4J_SSH", executable("RSYNC4J_SSH", "/usr/bin/ssh")),
        (
            "ssh-keygen",
            "RSYNC4J_SSHKEYGEN",
            executable("RSYNC4J_SSHKEYGEN", "/usr/bin/ssh-keygen"),
        ),
    ] {
        if !Path::new(&path).exists() {
            return Err(VendorError::Other(failure(
                "java.lang.IllegalStateException",
                format!(
                    "{name} not installed? Was looking for '{path}'. You can use environment variable {variable} to point to the binary."
                ),
            )));
        }
    }
    if !Path::new(&sshpass).exists() {
        use std::io::Write;
        let text = jcompat::writers::JavaString::from(format!(
            "WARNING: sshpass binary not installed? Was looking for '{sshpass}'. You can use environment variable RSYNC4J_SSHPASS to point to the binary.\n"
        ));
        let _ = std::io::stderr().write_all(&text.to_utf8_bytes());
    }
    *paths = Some(BinaryPaths { rsync, sshpass });
    Ok(())
}

impl Inner {
    fn arguments(&self, source: String, destination: String, receive: bool) -> VendorResult<Vec<String>> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            VendorError::Other(failure(
                "com.malinskiy.marathon.config.exceptions.ConfigurationException",
                "rsync bridge needs ssh auth",
            ))
        })?;
        let mut args = Vec::new();
        if let SshAuthentication::Password { password, .. } = auth {
            args.push(sshpass_program());
            if !password.to_string_lossy().is_empty() {
                args.extend(["-p".into(), password.to_string_lossy()]);
            }
        }
        args.push(rsync_program());
        if self.debug {
            args.push("--verbose".into());
        }
        args.extend(
            [
                "--recursive",
                "--links",
                "--perms",
                "--owner",
                "--group",
                "--devices",
                "--specials",
                "--times",
            ]
            .map(str::to_owned),
        );
        if !receive {
            args.push(format!("--timeout={SEND_IO_TIMEOUT_SECONDS}"));
        }
        let mut rsh = "ssh -o 'StrictHostKeyChecking no' -F /dev/null ".to_owned();
        let interval = self.ssh.keep_alive_interval.seconds();
        if interval > 0 {
            rsh.push_str(&format!(
                "-o ServerAliveInterval={interval} -o ServerAliveCountMax={} ",
                self.ssh.keep_alive_max_count
            ));
        }
        let username = match auth {
            SshAuthentication::Password { username, .. } => username,
            SshAuthentication::PublicKey { username, key } => {
                rsh.push_str(&format!("-i {} ", String::from_utf16_lossy(&key.0)));
                username
            }
        };
        rsh.push_str(&format!("-l {} -p {} ", username.to_string_lossy(), self.target.port));
        if self.debug && self.ssh.debug {
            rsh.push_str("-vvv");
        }
        args.extend(["--rsh".into(), rsh]);
        let remote_path = self.config.remote_path.to_string_lossy();
        if !remote_path.is_empty() {
            args.push(format!("--rsync-path={remote_path}"));
        }
        args.extend([
            "--delete".into(),
            "--partial".into(),
            "--partial-dir=.rsync-partial".into(),
            "--delay-updates".into(),
            source,
            destination,
        ]);
        Ok(args)
    }
    async fn process(&self, argv: Vec<String>, receive: bool) -> VendorResult<(i32, String, String)> {
        blocking(self.runtime.as_ref(), initialize_binaries).await?;
        let mut child = blocking(self.runtime.as_ref(), move || {
            Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(io_error)
        })
        .await?;
        // ProcessBuilder leaves stdin open while monitor waits; rsync does not read it.
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let readers = Arc::new(SessionScope::default());
        let out_scope = readers.clone();
        let (out_tx, out_rx) = oneshot::channel();
        let out_task = self.runtime.spawn(Box::pin(async move {
            let _ = out_tx.send(read_pipe(stdout, out_scope).await);
            Ok(())
        }));
        let err_scope = readers.clone();
        let (err_tx, err_rx) = oneshot::channel();
        let stall = Arc::new(Notify::new());
        let reported = (!receive).then(|| stall.clone());
        let err_task = self.runtime.spawn(Box::pin(async move {
            let _ = err_tx.send(read_stderr(stderr, err_scope, reported).await);
            Ok(())
        }));
        let scope = Arc::new(SessionScope::default());
        let mut timed_out = false;
        // Errors are held until the child is reaped and both readers are joined.
        let status = if receive {
            tokio::select! {
                result = child.wait() => result.map_err(io_error),
                result = self.clock.delay(30_000, scope) => {
                    let mut early = result.err();
                    let status = if early.is_none() {
                        timed_out = true;
                        let diagnostic = jcompat::writers::JavaString::from("Timeout of 30 seconds reached, terminating process...\n").to_utf8_bytes();
                        early = blocking(self.runtime.as_ref(), move || {
                            use std::io::Write;
                            let _ = std::io::stderr().write_all(&diagnostic);
                            Ok(())
                        }).await.err();
                        // U155: processoutput4j calls destroy() and then waits without a limit, and TERM to `sshpass` leaves
                        // rsync and ssh behind, so this stops the transfer as a stalled send is stopped.
                        self.stop_process(&mut child).await
                    } else {
                        child.wait().await.map_err(io_error)
                    };
                    early.map_or(status, Err)
                }
            }
        } else {
            tokio::select! {
                biased;
                result = child.wait() => result.map_err(io_error),
                _ = stall.notified() => self.stop_stalled_send(&mut child).await,
            }
        };
        if status.is_err() {
            // A failed wait may not have reaped the child; the error still wins.
            let _ = child.wait().await;
        }
        drop(stdin);
        if timed_out || status.is_err() {
            readers.cancel(crate::cmd::cancelled("Process output timed out"));
        }
        // D31, and U158 for a receive: a hung ssh that outlived rsync may hold these pipes open. What was read stays.
        let joined = async {
            let _ = out_task.join().await;
            let _ = err_task.join().await;
        };
        if tokio::time::timeout(PROCESS_LIMIT, joined).await.is_err() {
            readers.cancel(crate::cmd::cancelled("Process output outlived the process"));
        }
        let out_joined = out_task.join().await;
        let err_joined = err_task.join().await;
        let status = status?;
        out_joined?;
        err_joined?;
        let dropped = |e: tokio::sync::oneshot::error::RecvError| {
            VendorError::Other(failure("java.lang.IllegalStateException", e.to_string()))
        };
        use std::os::unix::process::ExitStatusExt;
        Ok((
            status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
            out_rx.await.map_err(dropped)??,
            err_rx.await.map_err(dropped)??,
        ))
    }
    /// D31: `--timeout` has expired, but rsync may not end by itself: openrsync closes ssh's input and then waits for ssh,
    /// for ever if ssh is hung. TERM ends openrsync and its ssh. `sshpass` is another matter: it exits on TERM at once and
    /// leaves rsync and a real ssh running, so under it rsync gets the TERM too, and its step before KILL. A process group
    /// would be simpler, but it would keep rsync from a terminal's Ctrl-C on every send. On return the child is reaped, so
    /// a retry cannot overlap it, except for an rsync under `sshpass` that ignores TERM when its identity can't be read
    /// again: it gets no KILL.
    async fn stop_stalled_send(&self, child: &mut Child) -> VendorResult<ExitStatus> {
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let clock = self.clock.as_ref();
        if let Some(status) = within(clock, scope.clone(), STOP_STEP_MILLIS, child.wait()).await {
            return status.map_err(io_error);
        }
        self.stop_process(child).await
    }
    /// [`Self::stop_stalled_send`] from the TERM on, which a receive at its deadline also goes through (U155).
    async fn stop_process(&self, child: &mut Child) -> VendorResult<ExitStatus> {
        use nix::{sys::signal, unistd::Pid};
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let clock = self.clock.as_ref();
        let alive = |pid: i32| signal::kill(Pid::from_raw(pid), None).is_ok();
        let mut under_sshpass = Vec::new();
        if let Some(pid) = child.id() {
            if matches!(self.auth, Some(SshAuthentication::Password { .. })) {
                under_sshpass = children_of(pid).await;
            }
            for pid in under_sshpass.iter().map(|(pid, _)| *pid).chain([pid as i32]) {
                let _ = signal::kill(Pid::from_raw(pid), signal::Signal::SIGTERM);
            }
        }
        let stopped = async {
            let _ = child.wait().await;
            while under_sshpass.iter().any(|(pid, _)| alive(*pid)) {
                if clock.delay(STOP_POLL_MILLIS, scope.clone()).await.is_err() {
                    break;
                }
            }
        };
        let _ = within(clock, scope.clone(), STOP_STEP_MILLIS, stopped).await;
        // Listed a step ago, so a pid can only be another process's if pids wrapped around in those few seconds, and then
        // it started later. A `ps` that cannot say either time leaves the listed process to KILL only if it could not say
        // at the listing, as on a host without `lstart`.
        for (pid, started) in under_sshpass.into_iter().filter(|(pid, _)| alive(*pid)) {
            let deadline = tokio::time::Instant::now() + PROCESS_LIMIT;
            if started.is_none() || started_at(pid, deadline).await == started {
                let _ = signal::kill(Pid::from_raw(pid), signal::Signal::SIGKILL);
            }
        }
        // Does nothing to a child that has been reaped, whose status `wait` then returns.
        let _ = child.start_kill();
        child.wait().await.map_err(io_error)
    }
    async fn transfer(&self, local: PathBuf, remote: String, receive: bool) -> VendorResult<bool> {
        let local = blocking(self.runtime.as_ref(), move || {
            let cwd = std::env::current_dir().map_err(io_error)?;
            let path = absolute_file_path(&local.to_string_lossy(), &cwd.to_string_lossy());
            let mut text = String::from_utf16_lossy(&path.0);
            if local.is_dir() {
                text = format!("{}/", text.trim_end_matches('/'));
            }
            Ok(text)
        })
        .await?;
        let remote = format!("{}:{remote}", self.target.addr.to_string_lossy());
        let (source, destination) = if receive { (remote, local) } else { (local, remote) };
        let (status, _, stderr) = self
            .process(self.arguments(source, destination.clone(), receive)?, receive)
            .await?;
        if status != 0 && !stderr.is_empty() {
            self.logging.log(
                &self.logger,
                Level::Error,
                || {
                    if receive {
                        stderr
                    } else if reports_stall(stderr.as_bytes()) {
                        format!(
                            "send error: no progress in {SEND_IO_TIMEOUT_SECONDS} s sending to {destination}, rsync stopped: {stderr}"
                        )
                    } else {
                        format!("send error: {stderr}")
                    }
                },
                None,
            );
        }
        Ok(status == 0)
    }
}
/// Best effort: no `pgrep`, or one that takes too long, only means the signals reach the child alone. Each child comes
/// with when it started, within the same limit, so that KILL can tell it from a later process with its pid.
async fn children_of(pid: u32) -> Vec<(i32, Option<String>)> {
    let deadline = tokio::time::Instant::now() + PROCESS_LIMIT;
    let listing = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let pids: Vec<i32> = match tokio::time::timeout_at(deadline, listing).await {
        Ok(Ok(output)) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect(),
        _ => Vec::new(),
    };
    let mut children = Vec::with_capacity(pids.len());
    for pid in pids {
        children.push((pid, started_at(pid, deadline).await));
    }
    children
}
/// `ps -o lstart=`: `None` for a process that is gone, or a `ps` that cannot say in time.
async fn started_at(pid: i32, deadline: tokio::time::Instant) -> Option<String> {
    let probe = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout_at(deadline, probe).await.ok()?.ok()?;
    let started = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !started.is_empty()).then_some(started)
}
fn reports_stall(stderr: &[u8]) -> bool {
    STALL_REPORTS
        .iter()
        .any(|report| stderr.windows(report.len()).any(|window| window == *report))
}
async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, stop: Arc<SessionScope>) -> VendorResult<String> {
    read_stderr(pipe, stop, None).await
}
/// `read_pipe`, also notifying `stall` once the text reports a stalled send. Where `stall` is set, the check runs over all
/// the text so far, since a report can span two reads; rsync says little on stderr without `--verbose`, which goes to stdout.
async fn read_stderr<R: tokio::io::AsyncRead + Unpin>(
    pipe: Option<R>,
    stop: Arc<SessionScope>,
    stall: Option<Arc<Notify>>,
) -> VendorResult<String> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        loop {
            let mut chunk = [0; 8192];
            let count = tokio::select! {
                biased;
                result = pipe.read(&mut chunk) => result.map_err(io_error)?,
                _ = stop.cancelled() => break,
            };
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(stall) = &stall
                && reports_stall(&bytes)
            {
                stall.notify_one();
            }
        }
    }
    // CollectingProcessReader uses BufferedReader.readLine and appends '\n' to every line.
    let text = jcompat::utf8::decode_utf8(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    Ok(if text.is_empty() || text.ends_with('\n') {
        text
    } else {
        text + "\n"
    })
}
impl FileBridge for RsyncFileBridge {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool> {
        let (inner, src, dst) = (self.0.clone(), src.to_owned(), dst.to_owned());
        Box::pin(async move {
            self.owned(async move {
                let _send = inner.send.lock().await;
                inner.transfer(src, dst, false).await
            })
            .await
        })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        let (inner, src, dst) = (self.0.clone(), src.to_owned(), dst.to_owned());
        Box::pin(async move { self.owned(async move { inner.transfer(dst, src, true).await }).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn started_at_tells_a_process_from_one_that_is_gone() {
        let deadline = || tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let own = std::process::id() as i32;
        let started = started_at(own, deadline()).await;
        assert!(started.is_some());
        assert_eq!(started_at(own, deadline()).await, started);
        let mut child = Command::new("/bin/sleep")
            .arg("100")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        assert!(started_at(pid, deadline()).await.is_some());
        child.kill().await.unwrap();
        assert_eq!(started_at(pid, deadline()).await, None);
    }
}
