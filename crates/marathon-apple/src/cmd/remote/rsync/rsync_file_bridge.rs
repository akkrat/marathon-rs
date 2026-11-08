//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt

use super::RsyncTarget;
use crate::cmd::{
    FileBridge, SessionScope, TransferStop, TransferStopped, blocking, failure, io_error,
    remote::ssh::sshj::{client::within, config::SshLogging},
    retained_tasks::RetainedTasks,
    stall_record::StallRecord,
};
use jcompat::{
    file_manager::absolute_file_path,
    logging::{Level, Logger},
};
use marathon_config::vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration};
use marathon_core::{
    error::{LeavePoolReason, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorRuntime},
};
use std::{
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Notify, oneshot},
};

/// D31, D43: a transfer fails after this long with no I/O. A limit on progress, not on the total, because a large bundle or
/// video over a slow link is legitimate. Long, because a worker can be silent for a while with the disk under load, as when
/// it is booting simulators, and failing a healthy transfer costs more than noticing a dead one later.
pub const TRANSFER_IO_TIMEOUT_SECONDS: i64 = 300;
/// The class of the failure a stalled send carries out of the bridge (U163).
pub const TRANSFER_STALLED: &str = "com.malinskiy.marathon.apple.cmd.remote.rsync.TransferStalled";
/// D31: rsync 3's exit status for an expired `--timeout`, `RERR_TIMEOUT`. It also ends `--stop-after` and `--stop-at` with it,
/// after `STOPPED_AT_LIMIT`. openrsync's status after its report depends on how ssh ends, so there only the report says it.
const RSYNC_TIMEOUT_STATUS: i32 = 30;
const STOPPED_AT_LIMIT: &str = "stopping at requested limit";
/// D31, D43: rsync's report or status counts as a stall only after this long with no progress the bridge saw: since the start
/// for a send, since its files last changed for a receive. That is `--timeout` less one look at those files, which may see a
/// change up to 10 s late, and 5 s for the clocks to differ. A failure that comes sooner is never a stall, whatever it says.
const STALL_AFTER_MILLIS: i64 = TRANSFER_IO_TIMEOUT_SECONDS * 1000 - RECEIVE_WATCH_MILLIS - 5_000;
/// D31, D43: how often the time with no progress is looked at. It is measured on the clock, so a late look loses nothing.
const QUIET_TICK_MILLIS: i64 = 1_000;
/// D31: how long a process that reported a stall gets to end at each step. It only has to outlast one on its way out.
const STOP_STEP_MILLIS: i64 = 5_000;
const STOP_POLL_MILLIS: i64 = 100;
/// D43: how often a receive looks at what has arrived. A pull that ends sooner never looks.
const RECEIVE_WATCH_MILLIS: i64 = 10_000;
/// D31: real time, not the vendor clock: this bounds how soon other processes get scheduled (`ps`, a pipe's last bytes),
/// which is no shorter in a test that compresses the clock.
const PROCESS_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);
/// D31, D43: when a running transfer's processes are listed, in real time like `PROCESS_LIMIT`. Once ssh is up, which is at
/// once, and then now and then for what starts later. A transfer that ends within the first never lists.
const LIST_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
const LIST_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

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
    stalls: StallRecord,
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
            stalls: StallRecord::default(),
            tasks: RetainedTasks::default(),
        }));
        if debug {
            let inner = bridge.0.clone();
            bridge
                .owned(async move {
                    let output = inner
                        .process(
                            vec![rsync_program(), "--version".into(), "/tmp".into(), "/tmp".into()],
                            None,
                            None,
                        )
                        .await?;
                    let first = output.stdout.split('\n').next().unwrap_or_default().to_owned();
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
    pub fn arguments(&self, source: String, destination: String) -> VendorResult<Vec<String>> {
        self.0.arguments(source, destination)
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
    fn arguments(&self, source: String, destination: String) -> VendorResult<Vec<String>> {
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
        args.push(format!("--timeout={TRANSFER_IO_TIMEOUT_SECONDS}"));
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
    /// `arriving` is where a receive's bytes land; `stop` ends it early (D54).
    async fn process(
        &self,
        argv: Vec<String>,
        arriving: Option<Arriving>,
        mut stop: Option<&mut TransferStop>,
    ) -> VendorResult<Outcome> {
        blocking(self.runtime.as_ref(), initialize_binaries).await?;
        // The start is read on the vendor clock as the process starts, not when the spawn's result reaches this task, so a
        // late delivery can't shorten the time a send has been quiet.
        let clock = self.clock.clone();
        let (mut child, started) = blocking(self.runtime.as_ref(), move || {
            let child = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(io_error)?;
            Ok((child, clock.now_millis()))
        })
        .await?;
        let descendants = Descendants::of(&child);
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
        let reported = stall.clone();
        let err_task = self.runtime.spawn(Box::pin(async move {
            let _ = err_tx.send(read_stderr(stderr, err_scope, Some(reported)).await);
            Ok(())
        }));
        // Errors are held until the child is reaped and both readers are joined.
        let progress = AtomicI64::new(started);
        let mut nothing_arrived = false;
        let mut stopped = None;
        let stopping = async {
            match stop.as_deref_mut() {
                Some(stop) => stop.stopped().await,
                None => std::future::pending().await,
            }
        };
        let status = tokio::select! {
            biased;
            result = child.wait() => result.map_err(io_error),
            arrived = self.watch(arriving, &stall, &progress) => {
                nothing_arrived = !arrived;
                self.stop_child(&mut child, &descendants, true).await
            }
            // D54: rsync has said nothing and is not on its way out, so it gets no time to end by itself.
            why = stopping => {
                stopped = Some(why);
                self.stop_child(&mut child, &descendants, false).await
            }
            _ = descendants.follow() => unreachable!("following never ends"),
        };
        // Measured as the process ended, however that came about, and before its pipes are waited for.
        let quiet = self.clock.now_millis() - progress.load(Ordering::SeqCst);
        if status.is_err() {
            // A failed wait may not have reaped the child; the error still wins.
            let _ = child.wait().await;
        }
        drop(stdin);
        if status.is_err() {
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
        let status = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        let stdout = out_rx.await.map_err(dropped)??;
        let stderr = err_rx.await.map_err(dropped)??;
        // D54: once a stop has been sent, what rsync started goes, whatever rsync's status says.
        let stop_sent = stopped.is_some();
        // D54: an rsync that finished all the same has done its transfer, and keeps its success.
        let stopped = stopped.filter(|_| status != 0);
        let stalled = status != 0
            && !stop_sent
            && (nothing_arrived
                || (quiet >= STALL_AFTER_MILLIS
                    && (reports_stall(stderr.as_bytes())
                        || (status == RSYNC_TIMEOUT_STATUS && !stderr.contains(STOPPED_AT_LIMIT)))));
        // However the stall was decided, and also when rsync ended by itself, what it started goes with it. So too on a stop.
        if stalled || stop_sent {
            descendants.end(self.clock.as_ref()).await;
        }
        Ok(Outcome {
            status,
            stdout,
            stderr,
            stalled,
            stopped,
        })
    }
    /// D31, D43: keeps in `progress` when progress was last seen, on the vendor clock: the start for a send, the last change of
    /// its own files for a receive, which are looked at every 10 s. Ends once rsync has reported a stall and the time since
    /// has reached `STALL_AFTER_MILLIS`, returning `true`, or once a receive's files have not changed for the limit, returning
    /// `false`: between two rsync 3 installs `--timeout` does not end a pull that has stopped, since the generator's
    /// keep-alives count as I/O, so it reports nothing and waits for ever where a total deadline used to end it. A clock that
    /// fails leaves the other limits in place.
    async fn watch(&self, arriving: Option<Arriving>, reported: &Notify, progress: &AtomicI64) -> bool {
        let arriving = arriving.map(Arc::new);
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let clock = self.clock.as_ref();
        let quiet = || clock.now_millis() - progress.load(Ordering::SeqCst);
        let (mut seen, mut looked, mut has_reported) = (None, clock.now_millis(), false);
        let mut tick = clock.delay(QUIET_TICK_MILLIS, scope.clone());
        loop {
            let ticked = tokio::select! {
                _ = reported.notified() => {
                    has_reported = true;
                    None
                }
                result = &mut tick => Some(result),
            };
            if let Some(result) = ticked {
                if result.is_err() {
                    return std::future::pending().await;
                }
                tick = clock.delay(QUIET_TICK_MILLIS, scope.clone());
                if let Some(arriving) = &arriving
                    && clock.now_millis() - looked >= RECEIVE_WATCH_MILLIS
                {
                    let arriving = arriving.clone();
                    let now = blocking(self.runtime.as_ref(), move || Ok(Arrived::at(&arriving)))
                        .await
                        .ok();
                    // Stamped once the look has returned: a look whose result came late saw files that changed by then.
                    looked = clock.now_millis();
                    if now != seen {
                        progress.store(looked, Ordering::SeqCst);
                    }
                    seen = now;
                    if quiet() >= TRANSFER_IO_TIMEOUT_SECONDS * 1000 {
                        return false;
                    }
                }
            }
            if has_reported && quiet() >= STALL_AFTER_MILLIS {
                return true;
            }
        }
    }
    /// D31, D43: rsync has stalled and may not end by itself: openrsync closes ssh's input and then waits for ssh, for ever if
    /// ssh is hung. So it gets 5 s if it has reported (`grace`), then TERM, together with what it started, since `sshpass`
    /// exits on TERM at once and would leave rsync running; then 5 s more and KILL. On return the child is reaped, so a retry
    /// cannot overlap it; what it started is `Descendants::end`'s, once the outcome is known.
    async fn stop_child(&self, child: &mut Child, descendants: &Descendants, grace: bool) -> VendorResult<ExitStatus> {
        use nix::{sys::signal, unistd::Pid};
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let clock = self.clock.as_ref();
        // Still unreaped here, so whatever is attached to it now is found.
        descendants.list(true).await;
        if grace && let Some(status) = within(clock, scope.clone(), STOP_STEP_MILLIS, child.wait()).await {
            return status.map_err(io_error);
        }
        // Unreaped, so its pid is still its own.
        if let Some(pid) = child.id() {
            let _ = signal::kill(Pid::from_raw(pid as i32), signal::Signal::SIGTERM);
        }
        descendants.signal(signal::Signal::SIGTERM, true).await;
        if let Some(status) = within(clock, scope, STOP_STEP_MILLIS, child.wait()).await {
            return status.map_err(io_error);
        }
        let _ = child.start_kill();
        child.wait().await.map_err(io_error)
    }
    async fn transfer(
        &self,
        local: PathBuf,
        remote: String,
        receive: bool,
        stop: &mut TransferStop,
    ) -> VendorResult<bool> {
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
        let arriving = receive.then(|| Arriving::new(&local, &remote));
        let remote = format!("{}:{remote}", self.target.addr.to_string_lossy());
        if let Some(stopped) = stop.now() {
            return self.stopped(stopped, receive, &remote);
        }
        let (source, destination) = if receive { (remote, local) } else { (local, remote) };
        let Outcome {
            status,
            stderr,
            stalled,
            stopped,
            ..
        } = self
            .process(
                self.arguments(source.clone(), destination.clone())?,
                arriving,
                Some(stop),
            )
            .await?;
        if let Some(stopped) = stopped {
            return self.stopped(stopped, receive, if receive { &source } else { &destination });
        }
        if stalled || (status != 0 && !stderr.is_empty()) {
            let text = stderr.clone();
            self.logging.log(
                &self.logger,
                Level::Error,
                || match (receive, stalled) {
                    (true, false) => text,
                    (true, true) => format!(
                        "receive error: no progress in {TRANSFER_IO_TIMEOUT_SECONDS} s receiving {source}, rsync stopped: {text}"
                    ),
                    (false, false) => format!("send error: {text}"),
                    (false, true) => format!(
                        "send error: no progress in {TRANSFER_IO_TIMEOUT_SECONDS} s sending to {destination}, rsync stopped: {text}"
                    ),
                },
                None,
            );
        }
        // U163: a send's own outcome, which nothing retries. A receive stays a `false` its listener deals with, since a pull
        // that failed costs an artifact and not the device.
        if stalled && !receive {
            let error = VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                detail: failure(
                    TRANSFER_STALLED,
                    format!(
                        "No progress in {TRANSFER_IO_TIMEOUT_SECONDS} s sending to {destination}, the device leaves its pool"
                    ),
                ),
            };
            self.stalls.record(error.clone());
            return Err(error);
        }
        Ok(status == 0)
    }
    /// D54: the outcome of a transfer to or from `remote` that `stopped` ended, or kept from starting. A cancellation is the
    /// caller's to report. A lost connection fails a send, which nothing retries on that device since its commands fail too,
    /// and leaves a receive the `false` its listener deals with, as D43 does.
    fn stopped(&self, stopped: TransferStopped, receive: bool, remote: &str) -> VendorResult<bool> {
        match stopped {
            TransferStopped::Cancelled(reason) => Err(VendorError::Cancelled(reason)),
            TransferStopped::Lost(lost) => {
                if self.logging.enabled(&self.logger, Level::Error) {
                    let message = if receive {
                        format!("receive error: the device's connection was lost while receiving {remote}")
                    } else {
                        format!("send error: the device's connection was lost while sending to {remote}")
                    };
                    (self.logging.sink)(&self.logger, Level::Error, message, Some(lost.clone()));
                }
                if receive {
                    Ok(false)
                } else {
                    Err(VendorError::Other(lost))
                }
            }
        }
    }
}
/// What a process did: its status, what it wrote, whether it stalled (D31, D43), and what stopped it early (D54).
struct Outcome {
    status: i32,
    stdout: String,
    stderr: String,
    stalled: bool,
    stopped: Option<TransferStopped>,
}
/// D43: where this receive's own bytes land, so that another transfer into the same folder cannot pass for its progress.
/// rsync writes `name` into `folder` through a temporary file beside it (`is_temporary_of`), moves an interrupted one to
/// `.rsync-partial/name`, and with
/// `--delay-updates` keeps a finished one there until the end. Under a folder that is being pulled everything is its own.
/// A source ending in `/` fills `folder` itself, and then the whole of it is looked at, so other writes there count.
enum Arriving {
    Named { folder: PathBuf, name: std::ffi::OsString },
    Everything(PathBuf),
}
impl Arriving {
    /// `destination` is the argument rsync gets, with a `/` at the end for a folder that exists; `source` is the remote path.
    fn new(destination: &str, source: &str) -> Self {
        let (folder, name) = match destination.strip_suffix('/') {
            Some(folder) => match source.trim_end_matches('/').rsplit('/').next() {
                Some(name) if !source.ends_with('/') && !name.is_empty() => {
                    (Path::new(folder), std::ffi::OsStr::new(name))
                }
                _ => return Self::Everything(PathBuf::from(destination)),
            },
            None => {
                let path = Path::new(destination);
                match path.file_name() {
                    Some(name) => (path.parent().unwrap_or(Path::new("")), name),
                    None => return Self::Everything(PathBuf::from(destination)),
                }
            }
        };
        let folder = if folder.as_os_str().is_empty() {
            Path::new(".")
        } else {
            folder
        };
        Self::Named {
            folder: folder.to_owned(),
            name: name.to_owned(),
        }
    }
}
/// The count, size and latest change of what has arrived.
#[derive(PartialEq)]
struct Arrived(u64, u64, Option<std::time::SystemTime>);
impl Arrived {
    fn at(arriving: &Arriving) -> Self {
        let mut arrived = Self(0, 0, None);
        match arriving {
            Arriving::Everything(folder) => arrived.add_tree(folder),
            Arriving::Named { folder, name } => {
                arrived.add_all(&folder.join(name));
                arrived.add_all(&folder.join(".rsync-partial").join(name));
                for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
                    if is_temporary_of(&entry.file_name(), name) {
                        arrived.add(&entry.path());
                    }
                }
            }
        }
        arrived
    }
    fn add(&mut self, path: &Path) {
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            self.0 += 1;
            self.1 += metadata.len();
            self.2 = self.2.max(metadata.modified().ok());
        }
    }
    fn add_all(&mut self, path: &Path) {
        self.add(path);
        if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
            self.add_tree(path);
        }
    }
    fn add_tree(&mut self, folder: &Path) {
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            self.add(&entry.path());
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                self.add_tree(&entry.path());
            }
        }
    }
}
/// rsync's temporary file for `name`: a prefix, a dot and random letters or digits. rsync 3 (`get_tmpname` in receiver.c, 3.4.1)
/// writes a dot, then the name less one leading dot, at most 246 bytes of it without splitting a multi-byte character, then
/// drops one trailing dot, which is that first dot when nothing of the name is left, and adds 6. openrsync, as measured on
/// macOS 27, writes a dot and the name as it is, cut to 243 bytes, and adds 10.
fn is_temporary_of(entry: &std::ffi::OsStr, name: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let (entry, name) = (entry.as_bytes(), name.as_bytes());
    let rsync = {
        let stem = name.strip_prefix(b".").unwrap_or(name);
        let stem = match stem.get(246) {
            Some(cut) => {
                let mut kept = 246;
                while cut & 0x80 != 0 && kept > 0 && stem[kept - 1] & 0x80 != 0 {
                    kept -= 1;
                }
                &stem[..kept]
            }
            None => stem,
        };
        let mut prefix = [b".", stem].concat();
        if prefix.ends_with(b".") {
            prefix.pop();
        }
        prefix
    };
    let openrsync = [b".", &name[..name.len().min(243)]].concat();
    !name.is_empty()
        && [(rsync, 6), (openrsync, 10)].into_iter().any(|(prefix, random)| {
            entry.len() == prefix.len() + 1 + random
                && entry.starts_with(&prefix)
                && entry[prefix.len()] == b'.'
                && entry[prefix.len() + 1..].iter().all(u8::is_ascii_alphanumeric)
        })
}
/// D31, D43: the processes a transfer started, each with when it started, as `ps` saw them. The child, then any process
/// whose parent is one of them. Seen while their parents were there, they stay known after those have gone.
struct Descendants {
    root: i32,
    seen: Mutex<std::collections::HashMap<i32, String>>,
}
impl Descendants {
    fn of(child: &Child) -> Self {
        Self {
            // A child that has exited before this keeps its pid until it is reaped, and has nothing to find.
            root: child.id().map_or(-1, |pid| pid as i32),
            seen: Mutex::new(std::collections::HashMap::new()),
        }
    }
    fn seen(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<i32, String>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Lists every process now and adds those that are this transfer's. `unreaped` says the child has not been reaped,
    /// so that its pid is still its own.
    async fn list(&self, unreaped: bool) -> Option<std::collections::HashMap<i32, (i32, String)>> {
        let listing = processes().await?;
        let mut seen = self.seen();
        if unreaped && let Some((_, started)) = listing.get(&self.root) {
            seen.entry(self.root).or_insert_with(|| started.clone());
        }
        loop {
            let found: Vec<(i32, String)> = listing
                .iter()
                .filter(|(pid, (parent, _))| {
                    !seen.contains_key(pid)
                        && seen
                            .get(parent)
                            .is_some_and(|started| listing.get(parent).is_some_and(|(_, now)| now == started))
                })
                .map(|(pid, (_, started))| (*pid, started.clone()))
                .collect();
            if found.is_empty() {
                break;
            }
            seen.extend(found);
        }
        Some(listing)
    }
    /// Sends `signal` to those seen that are still this transfer's, checked by a listing just before: the same pid with the
    /// same start, and a parent that is one of them or init, as for an orphan. The child itself is the caller's to signal,
    /// and counts as a parent only while `unreaped`. With no listing, nobody. Returns those signalled.
    async fn signal(&self, signal: nix::sys::signal::Signal, unreaped: bool) -> Vec<i32> {
        let Some(listing) = self.list(unreaped).await else {
            return Vec::new();
        };
        let seen = self.seen();
        let mut ours: std::collections::HashSet<i32> = std::collections::HashSet::new();
        if unreaped {
            ours.insert(self.root);
        }
        loop {
            let found: Vec<i32> = seen
                .iter()
                .filter(|(pid, started)| {
                    **pid != self.root
                        && !ours.contains(pid)
                        && listing
                            .get(pid)
                            .is_some_and(|(parent, now)| now == *started && (*parent == 1 || ours.contains(parent)))
                })
                .map(|(pid, _)| *pid)
                .collect();
            if found.is_empty() {
                break;
            }
            ours.extend(found);
        }
        drop(seen);
        ours.remove(&self.root);
        let ours: Vec<i32> = ours.into_iter().collect();
        for pid in &ours {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), signal);
        }
        ours
    }
    /// After a stall, with the child reaped: TERM to what it started, 5 s for them to go, then KILL to any still there.
    async fn end(&self, clock: &dyn VendorClock) {
        use nix::sys::signal::{Signal, kill};
        let termed = self.signal(Signal::SIGTERM, false).await;
        if termed.is_empty() {
            return;
        }
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let gone = async {
            while termed
                .iter()
                .any(|pid| kill(nix::unistd::Pid::from_raw(*pid), None).is_ok())
            {
                if clock.delay(STOP_POLL_MILLIS, scope.clone()).await.is_err() {
                    break;
                }
            }
        };
        if within(clock, scope.clone(), STOP_STEP_MILLIS, gone).await.is_none() {
            self.signal(Signal::SIGKILL, false).await;
        }
    }
    /// Lists while the transfer runs; never ends.
    async fn follow(&self) {
        tokio::time::sleep(LIST_FIRST).await;
        loop {
            self.list(true).await;
            tokio::time::sleep(LIST_EVERY).await;
        }
    }
}
/// `ps -A -o pid=,ppid=,lstart=`: every process by pid, with its parent and when it started, or `None` if `ps` fails or does
/// not answer within `PROCESS_LIMIT`.
async fn processes() -> Option<std::collections::HashMap<i32, (i32, String)>> {
    let listing = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,lstart="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PROCESS_LIMIT, listing).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut processes = std::collections::HashMap::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(pid), Some(parent)) = (words.next(), words.next()) else {
            continue;
        };
        let (Ok(pid), Ok(parent)) = (pid.parse(), parent.parse()) else {
            continue;
        };
        let started = words.collect::<Vec<_>>().join(" ");
        if !started.is_empty() {
            processes.insert(pid, (parent, started));
        }
    }
    Some(processes)
}
/// D31: whether a whole line of `stderr` is rsync's own report that `--timeout` expired: openrsync's
/// `rsync(<pid>): error: poll: timeout`, or rsync 3's `[<role>] io timeout after <n> seconds -- exiting`, without the role
/// before 3.1. The same words inside a file name or another message are not one. Only lines that have ended count.
fn reports_stall(stderr: &[u8]) -> bool {
    let Some(end) = stderr.iter().rposition(|b| *b == b'\n') else {
        return false;
    };
    stderr[..end].split(|b| *b == b'\n').any(|line| {
        let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line));
        is_openrsync_report(&line) || is_rsync_report(&line)
    })
}
fn is_openrsync_report(line: &str) -> bool {
    let Some((program, rest)) = line.split_once('(') else {
        return false;
    };
    let Some((pid, rest)) = rest.split_once(')') else {
        return false;
    };
    !program.is_empty()
        && !program.contains(char::is_whitespace)
        && !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && rest == ": error: poll: timeout"
}
fn is_rsync_report(line: &str) -> bool {
    let line = match line.strip_prefix('[') {
        Some(rest) => match rest.split_once("] ") {
            Some((role, rest)) if !role.is_empty() && role.bytes().all(|b| b.is_ascii_alphabetic()) => rest,
            _ => return false,
        },
        None => line,
    };
    line.strip_prefix("io timeout after ")
        .and_then(|rest| rest.strip_suffix(" seconds -- exiting"))
        .is_some_and(|seconds| !seconds.is_empty() && seconds.bytes().all(|b| b.is_ascii_digit()))
}
async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>, stop: Arc<SessionScope>) -> VendorResult<String> {
    read_stderr(pipe, stop, None).await
}
/// `read_pipe`, also notifying `stall` once the text reports a stalled transfer. Where `stall` is set, the check runs over all
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
        self.send_with(src, dst, TransferStop::never())
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        self.receive_with(src, dst, TransferStop::never())
    }
    fn send_with<'a>(&'a self, src: &'a Path, dst: &'a str, mut stop: TransferStop) -> VendorFuture<'a, bool> {
        let (inner, src, dst) = (self.0.clone(), src.to_owned(), dst.to_owned());
        Box::pin(async move {
            let arrived = inner.stalls.ticket();
            self.owned(async move {
                // D54: a send stopped while it waits for another's ends here, and the ones behind it go on.
                let _send = tokio::select! {
                    biased;
                    send = inner.send.lock() => send,
                    stopped = stop.stopped() => {
                        let remote = format!("{}:{dst}", inner.target.addr.to_string_lossy());
                        return inner.stopped(stopped, false, &remote);
                    }
                };
                // U163: this send waited out another's 300 s to the same worker, and would most likely spend as long itself.
                if let Some(error) = inner.stalls.since(arrived) {
                    inner.logging.log(
                        &inner.logger,
                        Level::Warn,
                        || format!("Not sending to {dst}: a send to this worker stalled while this one waited"),
                        None,
                    );
                    return Err(error);
                }
                inner.transfer(src, dst, false, &mut stop).await
            })
            .await
        })
    }
    fn receive_with<'a>(&'a self, src: &'a str, dst: &'a Path, mut stop: TransferStop) -> VendorFuture<'a, bool> {
        let (inner, src, dst) = (self.0.clone(), src.to_owned(), dst.to_owned());
        Box::pin(async move {
            self.owned(async move { inner.transfer(dst, src, true, &mut stop).await })
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn processes_tell_a_process_from_one_that_is_gone() {
        let own = std::process::id() as i32;
        let listing = processes().await.unwrap();
        let (parent, started) = listing.get(&own).unwrap().clone();
        assert_eq!(parent, nix::unistd::getppid().as_raw());
        assert_eq!(processes().await.unwrap().get(&own), Some(&(parent, started)));
        let mut child = Command::new("/bin/sleep")
            .arg("100")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        assert_eq!(
            processes().await.unwrap().get(&pid).map(|(parent, _)| *parent),
            Some(own)
        );
        child.kill().await.unwrap();
        assert_eq!(processes().await.unwrap().get(&pid), None);
    }

    #[test]
    fn only_rsyncs_own_timeout_reports_are_stalls_d31() {
        for report in [
            "rsync(90205): error: poll: timeout\n",
            "openrsync(7): error: poll: timeout\n",
            "[sender] io timeout after 300 seconds -- exiting\n",
            "[generator] io timeout after 300 seconds -- exiting\r\n",
            "[Receiver] io timeout after 3 seconds -- exiting\n",
            "io timeout after 300 seconds -- exiting\n",
            "rsync: some other failure\n[sender] io timeout after 300 seconds -- exiting\nrsync error: timeout in data send/receive (code 30) at io.c(201) [sender=3.4.1]\n",
        ] {
            assert!(reports_stall(report.as_bytes()), "{report:?}");
        }
        for other in [
            // The report's words in file names.
            "rsync: link_stat \"/tmp/marathon/rsync(1): error: poll: timeout\" failed: No such file or directory (2)\n",
            "rsync: [sender] link_stat \"/tmp/io timeout after 300 seconds -- exiting\" failed: No such file or directory (2)\n",
            "rsync(1): error: /tmp/poll: timeout: No such file or directory\n",
            "rsync(1): error: poll: timeout.txt: stat\n",
            "/tmp/x rsync(1): error: poll: timeout\n",
            "rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [generator=3.4.1]\n",
            // Other I/O errors.
            "rsync: [receiver] write error: Broken pipe (32)\nrsync error: error in rsync protocol data stream (code 12) at io.c(232) [receiver=3.4.1]\n",
            "rsync(1): error: poll: hangup on nonblocking write\n",
            "rsync(1): error: poll: bad fd\n",
            "ssh: connect to host 10.0.0.1 port 22: Operation timed out\nrsync: connection unexpectedly closed (0 bytes received so far) [sender]\n",
            "[sender] io timeout after 300 seconds -- exiting now\n",
            "[send er] io timeout after 300 seconds -- exiting\n",
            "io timeout after seconds -- exiting\n",
            // Not yet a whole line.
            "rsync(1): error: poll: timeout",
        ] {
            assert!(!reports_stall(other.as_bytes()), "{other:?}");
        }
    }

    #[test]
    fn a_receive_watches_its_own_name_its_temporary_files_and_its_partial_file_d43() {
        use std::ffi::OsStr;
        let named = |folder: &str, name: &str| (PathBuf::from(folder), OsStr::new(name).to_owned());
        let watch = |destination: &str, source: &str| match Arriving::new(destination, source) {
            Arriving::Named { folder, name } => Some((folder, name)),
            Arriving::Everything(_) => None,
        };
        assert_eq!(watch("/out/video.mp4", "/r/x.mp4"), Some(named("/out", "video.mp4")));
        assert_eq!(watch("video.mp4", "/r/x.mp4"), Some(named(".", "video.mp4")));
        assert_eq!(
            watch("/out/xcresult/", "/r/b.xcresult"),
            Some(named("/out/xcresult", "b.xcresult"))
        );
        assert_eq!(watch("/out/xcresult/", "/r/b.xcresult/"), None);
        let v = |count: usize| "v".repeat(count);
        let witness = format!("{}b", "a".repeat(200));
        let cut = format!("{}\u{e9}x", v(245));
        for (entry, name, temporary) in [
            // openrsync, as measured, and rsync 3.
            (".video.mp4.ACOHXjKp8w".to_owned(), "video.mp4".to_owned(), true),
            (".video.mp4.a1B2c3".into(), "video.mp4".into(), true),
            (".video.mp4.a1B2c".into(), "video.mp4".into(), false),
            (".video.mp4.a1B2c3d".into(), "video.mp4".into(), false),
            (".video.mp4.a1B-c3".into(), "video.mp4".into(), false),
            (".other.mp4.a1B2c3".into(), "video.mp4".into(), false),
            (".video.mp4.a1B2c3".into(), "video".into(), false),
            (".video.a1B2c3".into(), "video.mp4".into(), false),
            ("video.mp4.a1B2c3".into(), "video.mp4".into(), false),
            // rsync 3 drops one leading and one trailing dot; openrsync keeps both.
            (".back.a1B2c3".into(), ".back".into(), true),
            ("..back.ACOHXjKp8w".into(), ".back".into(), true),
            ("..back.a1B2c3".into(), ".back".into(), false),
            (".trail.a1B2c3".into(), "trail.".into(), true),
            (".trail..EAJLQpSxei".into(), "trail.".into(), true),
            (".trail..a1B2c3".into(), "trail.".into(), false),
            // Another file's name that the destination's starts with.
            (format!(".{}.a1B2c3", "a".repeat(200)), witness.clone(), false),
            (format!(".{witness}.a1B2c3"), witness.clone(), true),
            // Long names: rsync 3 keeps 246 bytes, openrsync 243, and rsync 3 doesn't split a character.
            (format!(".{}.a1B2c3", v(246)), v(250), true),
            (format!(".{}.ACOHXjKp8w", v(243)), v(250), true),
            (format!(".{}.a1B2c3", v(247)), v(250), false),
            (format!(".{}.a1B2c3", v(245)), cut.clone(), true),
            (format!(".{}\u{e9}.a1B2c3", v(245)), cut, false),
            // Nothing of the name left: rsync 3's trailing-dot rule takes its first dot too. A name of only a dot more.
            (".a1B2c3".into(), "\u{e9}".repeat(125), true),
            (".a1B2c3".into(), "video.mp4".into(), false),
            ("..a1B2c3".into(), "..".into(), true),
        ] {
            assert_eq!(
                is_temporary_of(OsStr::new(&entry), OsStr::new(&name)),
                temporary,
                "{entry} for {name}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let mine = Arriving::new(&dir.path().join("video.mp4").to_string_lossy(), "/r/video.mp4");
        let before = Arrived::at(&mine);
        // Another pull into the same folder, at each place it writes.
        std::fs::create_dir(dir.path().join(".rsync-partial")).unwrap();
        for other in ["log.txt", ".log.txt.a1B2c3", ".rsync-partial/log.txt"] {
            std::fs::write(dir.path().join(other), b"other").unwrap();
        }
        assert!(Arrived::at(&mine) == before);
        for (own, size) in [
            (".video.mp4.a1B2c3", 1),
            (".rsync-partial/video.mp4", 2),
            ("video.mp4", 3),
        ] {
            let before = Arrived::at(&mine);
            std::fs::write(dir.path().join(own), vec![0; size]).unwrap();
            assert!(Arrived::at(&mine) != before, "{own}");
        }
        // The review's witnesses: rsync 3's temporary file for `.back`, and another file's for a prefix of the name.
        let back = Arriving::new(&dir.path().join(".back").to_string_lossy(), "/r/back");
        let before = Arrived::at(&back);
        std::fs::write(dir.path().join(".back.a1B2c3"), b"x").unwrap();
        assert!(Arrived::at(&back) != before);
        let long = Arriving::new(&dir.path().join(&witness).to_string_lossy(), "/r/x");
        let before = Arrived::at(&long);
        std::fs::write(dir.path().join(format!(".{}.a1B2c3", "a".repeat(200))), b"x").unwrap();
        assert!(Arrived::at(&long) == before);
        // The review's round 4 witness: a name of which rsync 3 keeps nothing.
        let accents = Arriving::new(&dir.path().join("\u{e9}".repeat(125)).to_string_lossy(), "/r/x");
        let before = Arrived::at(&accents);
        std::fs::write(dir.path().join(".a1B2c3"), b"x").unwrap();
        assert!(Arrived::at(&accents) != before);
    }
}
