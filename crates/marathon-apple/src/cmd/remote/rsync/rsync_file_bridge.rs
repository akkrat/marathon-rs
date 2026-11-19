//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/rsync/RsyncFileBridge.kt

use marathon_logging::{Component, Level};

use super::RsyncTarget;
use crate::cmd::{
    FileBridge, SessionScope, TransferStop, TransferStopped, blocking, failure, io_error,
    remote::ssh::sshj::config::SshLogging, retained_tasks::RetainedTasks, stall_record::StallRecord,
};
use jcompat::file_manager::absolute_file_path;
use marathon_config::vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration};
use marathon_core::{
    error::{Cancellation, Failure, LeavePoolReason, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorClock, VendorFuture, VendorRuntime},
};
use marathon_error::{AppleKind, FailureKind};
use marathon_process::{Child, Command};
use std::{
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI64, AtomicU8, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    sync::{Notify, oneshot},
};

/// D31, D43: a transfer fails after this long with no I/O. A limit on progress, not on the total, because a large bundle or
/// video over a slow link is legitimate. Long, because a worker can be silent for a while with the disk under load, as when
/// it is booting simulators, and failing a healthy transfer costs more than noticing a dead one later.
pub const TRANSFER_IO_TIMEOUT_SECONDS: i64 = 300;
/// The class of the failure a stalled send carries out of the bridge (U163).
pub const TRANSFER_STALLED: FailureKind = FailureKind::Apple(AppleKind::TransferStalled);
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
/// D91: from a stop (D54) to the end of its cleanup, confirmed or not, on the vendor clock: D54's 15 s of steps, 20 s for
/// the process listings where `ps` is slow, and 5 s for the last reap. Each step gets what is left of it at most, and less
/// if the device that started the transfer needs it to have ended sooner.
pub const STOP_CLEANUP_MILLIS: i64 = 40_000;
/// D91: how long a process that was sent KILL gets to be reaped, in real time like `PROCESS_LIMIT`, since the kernel
/// decides it.
const REAP_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

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
    logger: Component,
    send: tokio::sync::Mutex<()>,
    stalls: StallRecord,
    tasks: RetainedTasks,
    /// D91: destinations of transfers whose end could not be confirmed, and the starts of rsync under way. Held only for
    /// a check or a change, never across a blocking call, so neither a start that hangs nor a stop waits on it.
    admission: Mutex<Admission>,
    /// D91: called on the blocking thread just before each call there that a filesystem can hold (tests only).
    hook: HookSlot,
}
/// D91: the calls a transfer makes on a blocking thread that a filesystem that does not answer can hold.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockingCall {
    /// Looking for rsync, ssh, ssh-keygen and sshpass.
    Probe,
    /// Starting rsync.
    SpawnRsync,
    /// After a start of rsync, with its pid: committed to its transfer, or killed since it was refused meanwhile.
    SpawnedRsync(Option<u32>),
    /// Starting `ps` for a process listing.
    SpawnPs,
}
#[cfg(feature = "test-support")]
type BlockingHook = Arc<dyn Fn(BlockingCall) + Send + Sync>;
#[cfg(feature = "test-support")]
type HookSlot = OnceLock<BlockingHook>;
/// Without the `test-support` feature there is no hook, and calling it does nothing.
#[cfg(not(feature = "test-support"))]
type HookSlot = ();
/// The hook as a blocking thread calls it.
#[derive(Clone, Default)]
struct Hook {
    #[cfg(feature = "test-support")]
    hook: Option<BlockingHook>,
}
impl Hook {
    #[cfg_attr(not(feature = "test-support"), allow(clippy::let_unit_value, clippy::unused_unit))]
    fn of(slot: &HookSlot) -> Self {
        #[cfg(feature = "test-support")]
        {
            Self {
                hook: slot.get().cloned(),
            }
        }
        #[cfg(not(feature = "test-support"))]
        {
            let () = *slot;
            Self {}
        }
    }
    fn call(&self, call: BlockingCall) {
        #[cfg(feature = "test-support")]
        if let Some(hook) = &self.hook {
            hook(call);
        }
        #[cfg(not(feature = "test-support"))]
        let _ = call;
    }
}
#[derive(Default)]
struct Admission {
    quarantined: std::collections::HashSet<String>,
    /// Each rsync start under way, by its destination.
    starting: Vec<(String, Arc<SpawnTicket>)>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiveName {
    Source,
    Bundle,
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
        let logger = logging.component("marathon_apple::cmd::remote::rsync::rsync_file_bridge");
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
            admission: Mutex::default(),
            hook: HookSlot::default(),
        }));
        if debug {
            let inner = bridge.0.clone();
            bridge
                .owned(async move {
                    let output = inner
                        .process(
                            vec![rsync_program(), "--version".into(), "/tmp".into(), "/tmp".into()],
                            None,
                            false,
                            &Stopping::new(None),
                            None,
                            None,
                            &mut None,
                        )
                        .await?;
                    let first = output.stdout.split('\n').next().unwrap_or_default().to_owned();
                    inner.logging.log(&inner.logger, Level::DEBUG, || first, None);
                    Ok(true)
                })
                .await?;
        }
        Ok(bridge)
    }
    async fn owned(&self, body: impl Future<Output = VendorResult<bool>> + Send + 'static) -> VendorResult<bool> {
        let (tx, rx) = oneshot::channel();
        let task = self.0.tasks.spawn(self.0.runtime.as_ref(), async move {
            // D91: the body, and with it a transfer's stop, is dropped before its result is sent, so whoever hears the
            // result finds the device's receipt already ended.
            let result = body.await;
            let _ = tx.send(result);
            Ok(())
        });
        task.join().await?;
        rx.await
            .map_err(|e| VendorError::Other(failure(FailureKind::IllegalState, e.to_string())))?
    }
    fn receive_staged<'a>(
        &'a self,
        src: &'a str,
        dst: &'a Path,
        stop: TransferStop,
        name: ReceiveName,
    ) -> VendorFuture<'a, bool> {
        let src = if name == ReceiveName::Bundle {
            src.trim_end_matches('/')
        } else {
            src
        };
        let (inner, src, dst) = (self.0.clone(), src.to_owned(), dst.to_owned());
        Box::pin(async move {
            self.owned(async move { inner.transfer(dst, src, Some(name), stop).await })
                .await
        })
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
    /// D91: `hook` is called on the blocking thread just before each `BlockingCall`, so that a test can hold one there as a
    /// filesystem that no longer answers would. Only the first one set counts.
    #[doc(hidden)]
    #[cfg(feature = "test-support")]
    pub fn on_blocking_call(&self, hook: Arc<dyn Fn(BlockingCall) + Send + Sync>) {
        let _ = self.0.hook.set(hook);
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
    sshpass_missing: bool,
}
// rsync4j's Binaries caches discovery per process, independently of bridge instances. D91: the look runs on a blocking
// thread, and whoever waits for it waits asynchronously, within its own stop; nothing holds a lock while it looks, so a
// look held by a mount that no longer answers holds up no other transfer, which looks again.
static BINARIES: tokio::sync::OnceCell<BinaryPaths> = tokio::sync::OnceCell::const_new();
fn executable(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.into())
}
fn rsync_program() -> String {
    BINARIES
        .get()
        .map(|p| p.rsync.clone())
        .unwrap_or_else(|| executable("RSYNC4J_RSYNC", "/usr/bin/rsync"))
}
fn sshpass_program() -> String {
    BINARIES
        .get()
        .map(|p| p.sshpass.clone())
        .unwrap_or_else(|| executable("RSYNC4J_SSHPASS", "/usr/bin/sshpass"))
}
fn probe_binaries() -> VendorResult<BinaryPaths> {
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
                FailureKind::IllegalState,
                format!(
                    "{name} not installed? Was looking for '{path}'. You can use environment variable {variable} to point to the binary."
                ),
            )));
        }
    }
    let sshpass_missing = !Path::new(&sshpass).exists();
    Ok(BinaryPaths {
        rsync,
        sshpass,
        sshpass_missing,
    })
}
/// rsync4j's warning when `sshpass` is missing, the line the differential comparison expects once on stderr (U123). D91:
/// written once, after the look has been published, from a thread of its own, so that a stderr nobody reads holds up
/// neither the look nor any transfer.
fn warn_sshpass_missing(sshpass: &str) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if WARNED.swap(true, Ordering::AcqRel) {
        return;
    }
    let text = format!(
        "WARNING: sshpass binary not installed? Was looking for '{sshpass}'. You can use environment variable RSYNC4J_SSHPASS to point to the binary.\n"
    );
    let _ = std::thread::Builder::new()
        .name("rsync4j-warning".into())
        .spawn(move || {
            use std::io::Write;
            let _ = std::io::stderr().write_all(text.as_bytes());
        });
}
/// D91: one start of rsync, from its admission to its commit. `phase` is where the starting thread is; `REFUSED` is set by
/// the transfer that gives up waiting for it, or with `DOOMED` by the quarantine of its destination. Whichever comes
/// first decides: a start that is refused before it begins never begins, and one refused after is killed at its commit.
struct SpawnTicket(AtomicU8);
const SPAWN_PENDING: u8 = 0;
const SPAWN_STARTED: u8 = 1;
const SPAWN_COMMITTED: u8 = 2;
const SPAWN_PHASE: u8 = 0x0f;
const SPAWN_REFUSED: u8 = 0x10;
const SPAWN_DOOMED: u8 = 0x20;
impl SpawnTicket {
    fn begin(&self) -> bool {
        self.0
            .compare_exchange(SPAWN_PENDING, SPAWN_STARTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn commit(&self) -> bool {
        self.0
            .compare_exchange(SPAWN_STARTED, SPAWN_COMMITTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn doomed(&self) -> bool {
        self.0.load(Ordering::Acquire) & SPAWN_DOOMED != 0
    }
    /// Refuses it if it has not begun, and says whether it did.
    fn refuse_if_pending(&self) -> bool {
        self.0
            .compare_exchange(
                SPAWN_PENDING,
                SPAWN_PENDING | SPAWN_REFUSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
    /// Refuses it, and says how far it had got.
    fn refuse(&self, doom: bool) -> u8 {
        let flags = if doom {
            SPAWN_REFUSED | SPAWN_DOOMED
        } else {
            SPAWN_REFUSED
        };
        self.0.fetch_or(flags, Ordering::AcqRel) & SPAWN_PHASE
    }
}

impl Inner {
    fn arguments(&self, source: String, destination: String) -> VendorResult<Vec<String>> {
        let auth = self.auth.as_ref().ok_or_else(|| {
            VendorError::Other(failure(
                FailureKind::Config(marathon_error::ConfigKind::Rejected),
                "rsync bridge needs ssh auth",
            ))
        })?;
        let mut args = Vec::new();
        if let SshAuthentication::Password { password, .. } = auth {
            args.push(sshpass_program());
            if !password.to_owned().is_empty() {
                args.extend(["-p".into(), password.to_owned()]);
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
                rsh.push_str(&format!("-i {} ", key.to_string_lossy().into_owned()));
                username
            }
        };
        rsh.push_str(&format!("-l {} -p {} ", username.to_owned(), self.target.port));
        if self.debug && self.ssh.debug {
            rsh.push_str("-vvv");
        }
        args.extend(["--rsh".into(), rsh]);
        let remote_path = self.config.remote_path.to_owned();
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
    /// `arriving` is where a receive's bytes land; `wrapped` says the process is `sshpass`. D91: every wait is a step of
    /// `stopping`'s (`Bound::run`), so a stop that comes at any point, before rsync starts, while it runs, while a stall is
    /// being ended or while its output is read, takes the transfer to the stop's cleanup within its deadline. rsync starts
    /// only if neither a stop nor the quarantine of `place` came first, checked under the quarantine's lock. `unconfirmed`
    /// gets what could not be confirmed about the end of the process and what it started, whatever is returned.
    #[allow(clippy::too_many_arguments)]
    async fn process(
        &self,
        argv: Vec<String>,
        arriving: Option<Arriving>,
        wrapped: bool,
        stopping: &Arc<Stopping>,
        cancelled: Option<Box<dyn Fn() -> Option<Cancellation> + Send>>,
        place: Option<String>,
        unconfirmed: &mut Option<Failure>,
    ) -> VendorResult<Outcome> {
        let bound = Bound {
            clock: self.clock.as_ref(),
            stopping: Some(stopping),
            runtime: self.runtime.as_ref(),
            hook: &self.hook,
        };
        // D93 (round 9): what refused the start, if its admission did (a device's closed pulls), so that the caller hears
        // that refusal rather than a stop without a reason.
        let refused: Arc<Mutex<Option<Cancellation>>> = Arc::default();
        let not_started = || {
            Outcome::not_started(stopping.stopped().or_else(|| {
                refused
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .map(TransferStopped::Cancelled)
            }))
        };
        let probe = {
            let (runtime, hook) = (self.runtime.clone(), Hook::of(&self.hook));
            BINARIES.get_or_try_init(move || async move {
                blocking(runtime.as_ref(), move || {
                    hook.call(BlockingCall::Probe);
                    probe_binaries()
                })
                .await
            })
        };
        match bound.run(None, None, true, probe).await {
            Step::Done(result) => {
                let paths = result?;
                if paths.sshpass_missing {
                    warn_sshpass_missing(&paths.sshpass);
                }
            }
            Step::Stopped | Step::Expired => return Ok(not_started()),
        }
        // D91: admitted in one short check under the admission lock (no stop, no quarantine), with a ticket for the start
        // that the quarantine can refuse; rsync then starts outside the lock.
        let ticket = Arc::new(SpawnTicket(AtomicU8::new(SPAWN_PENDING)));
        {
            let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(reason) = cancelled.as_ref().and_then(|cancelled| cancelled()) {
                *refused.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
            }
            if stopping.stopped().is_some() || refused.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                return Ok(not_started());
            }
            if let Some(place) = &place {
                if admission.quarantined.contains(place) {
                    return Ok(Outcome::quarantined());
                }
                admission.starting.push((place.clone(), ticket.clone()));
            }
        }
        // The start is read on the vendor clock as the process starts, not when the spawn's result reaches this task, so a
        // late delivery can't shorten the time a send has been quiet.
        let clock = self.clock.clone();
        let (starting, seen, hook, refusing) =
            (ticket.clone(), stopping.clone(), Hook::of(&self.hook), refused.clone());
        let spawn = blocking(self.runtime.as_ref(), move || {
            let reason = cancelled.as_ref().and_then(|cancelled| cancelled());
            let stopped = seen.stopped().is_some() || reason.is_some();
            if let Some(reason) = reason {
                *refusing.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
            }
            if stopped {
                starting.refuse(false);
            }
            if !starting.begin() {
                return Ok(None);
            }
            hook.call(BlockingCall::SpawnRsync);
            // Killed if it is ever dropped unreaped: when its end could not be confirmed, or its start came too late.
            let child = Command::new(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(io_error)?;
            let started = clock.now_millis();
            let pid = child.id();
            let spawned = if starting.commit() {
                Some((child, started))
            } else {
                drop(child);
                None
            };
            hook.call(BlockingCall::SpawnedRsync(pid));
            Ok(spawned)
        });
        let mut spawn = std::pin::pin!(spawn);
        let mut step = bound.run(None, None, true, spawn.as_mut()).await;
        // A stop before the start began gives it up; one after it began waits for it, within the deadline, to stop it.
        if matches!(step, Step::Stopped) && !ticket.refuse_if_pending() {
            step = bound.run(None, None, false, spawn.as_mut()).await;
        }
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .starting
            .retain(|(_, started)| !Arc::ptr_eq(started, &ticket));
        let (mut child, started) = match step {
            Step::Done(spawned) => match spawned? {
                Some(spawned) => spawned,
                None if ticket.doomed() => return Ok(Outcome::quarantined()),
                None => return Ok(not_started()),
            },
            Step::Stopped => return Ok(not_started()),
            // Stopped, and the start, begun, had not come back by the deadline: it is killed when it does.
            Step::Expired => {
                if ticket.refuse(false) != SPAWN_PENDING {
                    *unconfirmed = Some(failure(
                        FailureKind::Io,
                        format!(
                            "rsync to or from {} was starting when the time for its stop ran out",
                            self.target.addr.to_owned()
                        ),
                    ));
                }
                return Ok(not_started());
            }
        };
        let pid = child.id();
        let descendants = Descendants::of(&child);
        // ProcessBuilder leaves stdin open while monitor waits; rsync does not read it.
        let stdin = child.take_stdin();
        let stdout = child.take_stdout().map_err(io_error)?;
        let stderr = child.take_stderr().map_err(io_error)?;
        let readers = Arc::new(SessionScope::default());
        let out_scope = readers.clone();
        let (out_tx, mut out_rx) = oneshot::channel();
        let out_task = self.runtime.spawn(Box::pin(async move {
            let _ = out_tx.send(read_pipe(stdout, out_scope).await);
            Ok(())
        }));
        let err_scope = readers.clone();
        let (err_tx, mut err_rx) = oneshot::channel();
        let stall = Arc::new(Notify::new());
        let reported = stall.clone();
        let err_task = self.runtime.spawn(Box::pin(async move {
            let _ = err_tx.send(read_stderr(stderr, err_scope, Some(reported)).await);
            Ok(())
        }));
        // Errors are held until the child is reaped and both readers are joined.
        let receiving = arriving.is_some();
        let progress = AtomicI64::new(started);
        let mut nothing_arrived = false;
        let unbounded = Bound {
            stopping: None,
            ..bound
        };
        let (status, wrapper_stopped, stop_branch) = tokio::select! {
            biased;
            result = child.wait() => (Some(result.map_err(io_error)), false, false),
            arrived = self.watch(arriving, &stall, &progress) => {
                nothing_arrived = !arrived;
                // D31's steps, each held to the stop's deadline once a stop comes; the 5 s before TERM ends on one.
                let (status, wrapper_stopped) = self.stop_child(&mut child, &descendants, true, wrapped, bound).await;
                (status, wrapper_stopped, false)
            }
            // D54: rsync has said nothing and is not on its way out, so it gets no time to end by itself.
            _ = stopping.wait() => {
                let (status, wrapper_stopped) = self.stop_child(&mut child, &descendants, false, wrapped, bound).await;
                (status, wrapper_stopped, true)
            }
            _ = descendants.follow(unbounded) => unreachable!("following never ends"),
        };
        // Measured as the process ended, however that came about, and before its pipes are waited for.
        let quiet = self.clock.now_millis() - progress.load(Ordering::SeqCst);
        let mut doubts = Vec::new();
        let exited = match &status {
            Some(Ok(_)) => true,
            // A failed wait may not have reaped the child; the error still wins.
            Some(Err(_)) => bound
                .run(Some(REAP_LIMIT), None, false, child.wait())
                .await
                .done()
                .is_some_and(|waited| waited.is_ok()),
            None => false,
        };
        let reaped = exited && child.cleanup_confirmed();
        if !exited {
            doubts.push("it was not reaped".to_owned());
        } else if !reaped {
            doubts.push("its process group was not confirmed to have ended".to_owned());
        }
        // D54: once a stop has been sent, what rsync started goes, whatever rsync's status says. D91: before its pipes are
        // waited for, since that is what holds them; and then it is checked that none of it is left.
        let mut left = None;
        if bound.stopped() || !reaped {
            left = Some(descendants.end(!reaped, bound).await);
        }
        drop(stdin);
        if !matches!(status, Some(Ok(_))) {
            readers.cancel(crate::cmd::cancelled("Process output timed out"));
        }
        // D31, and U158 for a receive: a hung ssh that outlived rsync may hold these pipes open. What was read stays.
        let join = || async { (out_task.join().await, err_task.join().await) };
        let mut joined = None;
        let mut windows = 0;
        while joined.is_none() && windows < 2 {
            match bound.run(Some(PROCESS_LIMIT), None, left.is_none(), join()).await {
                Step::Done(both) => joined = Some(both),
                // D91: a stop while the output is read ends what rsync started first, which is what holds the pipes.
                Step::Stopped => left = Some(descendants.end(!reaped, bound).await),
                Step::Expired => {
                    windows += 1;
                    readers.cancel(crate::cmd::cancelled("Process output outlived the process"));
                }
            }
        }
        if joined.is_none() {
            doubts.push("its output was still being read".to_owned());
        } else if receiving
            && windows > 0
            && left.is_none()
            && status
                .as_ref()
                .is_some_and(|result| result.as_ref().is_ok_and(ExitStatus::success))
        {
            doubts.push("its output pipes did not close with the receive".to_owned());
        }
        let dropped = |e: tokio::sync::oneshot::error::TryRecvError| {
            VendorError::Other(failure(FailureKind::IllegalState, e.to_string()))
        };
        // What the readers had when they ended; nothing if they have not.
        let (stdout, stderr) = match &joined {
            Some(_) => (out_rx.try_recv().map_err(dropped), err_rx.try_recv().map_err(dropped)),
            None => (Ok(Ok(String::new())), Ok(Ok(String::new()))),
        };
        use std::os::unix::process::ExitStatusExt;
        let code = |status: &ExitStatus| status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        let status = match status {
            Some(Ok(status)) => Ok(code(&status)),
            Some(Err(error)) => Err(error),
            // D91: sent KILL and not reaped in time.
            None => Ok(128 + nix::sys::signal::Signal::SIGKILL as i32),
        };
        // U181: sshpass 1.10 can exit 0 at once on a TERM of its own, whatever rsync then does, so that 0 is not rsync's.
        let status = status.map(|status| match status {
            0 if wrapper_stopped => 128 + nix::sys::signal::Signal::SIGTERM as i32,
            status => status,
        });
        let err_text = match &stderr {
            Ok(Ok(text)) => text.clone(),
            _ => String::new(),
        };
        let failed = status.as_ref().map_or(true, |status| *status != 0);
        // D54: a stop decides only if it ended rsync; D31: a stall decided before any stop stays one.
        let stalled = !stop_branch
            && status
                .as_ref()
                .is_ok_and(|status| is_stall(*status, &err_text, quiet, nothing_arrived));
        // D54: an rsync that finished all the same has done its transfer, and keeps its success.
        let stopped = stopping.stopped().filter(|_| failed && !stalled);
        // However the stall was decided, and also when rsync ended by itself, what it started goes with it. D91: so too
        // for a stop that came at any point; only this check confirms an end after a stop.
        if (stalled || bound.stopped()) && left.is_none() {
            left = Some(descendants.end(!reaped, bound).await);
        }
        if let Some(left) = left {
            match left {
                Some(left) if left.is_empty() => {}
                Some(left) => doubts.push(format!(
                    "{} it started {} still running: {}",
                    if left.len() == 1 { "a process" } else { "processes" },
                    if left.len() == 1 { "is" } else { "are" },
                    left.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
                )),
                None => doubts.push("the processes it started could not be listed".to_owned()),
            }
        }
        if !doubts.is_empty() {
            let pid = pid.map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
            *unconfirmed = Some(failure(
                FailureKind::Io,
                format!(
                    "rsync (pid {pid}) to or from {} is not confirmed to have ended: {}",
                    self.target.addr.to_owned(),
                    doubts.join("; ")
                ),
            ));
        }
        let status = status?;
        if let Some((out_joined, err_joined)) = joined {
            out_joined?;
            err_joined?;
        }
        let stdout = stdout??;
        let stderr = stderr??;
        Ok(Outcome {
            status,
            stdout,
            stderr,
            stalled,
            stopped,
            quarantined: false,
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
    /// may exit on TERM at once and leave rsync running; then 5 s more and KILL. What it started is `Descendants::end`'s,
    /// once the outcome is known. D91: every step is one of `bound`'s, so once the transfer is stopped they are held to
    /// its deadline and the 5 s of grace end at once; the reap after KILL has `REAP_LIMIT` too, and the status is `None`
    /// for a child not reaped by then. Otherwise it is reaped, so a retry cannot overlap it.
    ///
    /// Under `sshpass` (`wrapped`) the TERM goes only to what it started, and `sshpass` then exits with rsync's own status
    /// (U181): 1.10 exits 0 at once on a TERM of its own before ssh has prompted, as when ssh hung, without waiting for
    /// rsync, where 1.09 passes it on and waits, and 1.06 dies of it. Only where no listing found them does `sshpass` get
    /// the TERM, and then the returned flag says so.
    async fn stop_child(
        &self,
        child: &mut Child,
        descendants: &Descendants,
        grace: bool,
        wrapped: bool,
        bound: Bound<'_>,
    ) -> (Option<VendorResult<ExitStatus>>, bool) {
        use nix::{sys::signal, unistd::Pid};
        // Still unreaped here, so whatever is attached to it now is found.
        descendants.list(true, bound).await;
        if grace && let Step::Done(status) = bound.run(None, Some(STOP_STEP_MILLIS), true, child.wait()).await {
            return (Some(status.map_err(io_error)), false);
        }
        // Unreaped, so its pid is still its own.
        let term_child = || {
            if let Some(pid) = child.id() {
                let _ = signal::kill(Pid::from_raw(pid as i32), signal::Signal::SIGTERM);
            }
        };
        if !wrapped {
            let _ = child.terminate();
        }
        let reached = if wrapped {
            descendants.signal(signal::Signal::SIGTERM, true, bound).await
        } else {
            Vec::new()
        };
        let wrapper_stopped = wrapped && reached.is_empty();
        if wrapper_stopped {
            term_child();
        }
        if let Step::Done(status) = bound.run(None, Some(STOP_STEP_MILLIS), false, child.wait()).await {
            return (Some(status.map_err(io_error)), wrapper_stopped);
        }
        let _ = child.start_kill();
        let reaped = bound.run(Some(REAP_LIMIT), None, false, child.wait()).await.done();
        (reaped.map(|status| status.map_err(io_error)), wrapper_stopped)
    }
    /// D91: one task watches `stop` for the transfer's whole life, so that every step of it sees a stop whenever it comes.
    async fn transfer(
        &self,
        local: PathBuf,
        remote: String,
        publication: Option<ReceiveName>,
        mut stop: TransferStop,
    ) -> VendorResult<bool> {
        let receive = publication.is_some();
        let mut stage = None;
        let stopping = Stopping::new(stop.receipt());
        // D93 (round 8): as rsync starts, its admission too (a device's closed pulls).
        let cancelled = stop.start_check();
        let mut unconfirmed = None;
        let result = {
            let clock = self.clock.clone();
            let pull = remote.clone();
            let watching = async {
                let why = stop.stopped().await;
                if receive && why == TransferStopped::Cancelled(crate::cmd::TransferReceipt::pull_deadline_reason()) {
                    self.logging.log(
                        &self.logger,
                        Level::WARN,
                        || {
                            format!(
                                "Stopping artifact pull of {pull} from {} within the device shutdown window",
                                self.target.addr
                            )
                        },
                        None,
                    );
                }
                stopping.stop(why, clock.now_millis());
                std::future::pending::<()>().await
            };
            let steps = self.transfer_steps(
                local,
                remote,
                publication,
                &stopping,
                Box::new(cancelled),
                &mut unconfirmed,
                &mut stage,
            );
            tokio::select! {
                biased;
                _ = watching => unreachable!("watching never ends"),
                result = steps => result,
            }
        };
        if let Some(why) = unconfirmed {
            stop.unconfirmed(why);
        }
        let Some(stage) = stage else { return result };
        if !matches!(result, Ok(true)) {
            return blocking(self.runtime.as_ref(), move || {
                stage.discard();
                result
            })
            .await;
        }
        let check = stop.cancellation_check();
        blocking(self.runtime.as_ref(), move || {
            if stop.connection_stop().is_some() {
                stage.discard();
                return Ok(false);
            }
            stage.publish(check)?;
            Ok(true)
        })
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn transfer_steps(
        &self,
        local: PathBuf,
        remote: String,
        publication: Option<ReceiveName>,
        stopping: &Arc<Stopping>,
        cancelled: Box<dyn Fn() -> Option<Cancellation> + Send + Sync>,
        unconfirmed: &mut Option<Failure>,
        stage: &mut Option<crate::cmd::artifact_stage::ArtifactStage>,
    ) -> VendorResult<bool> {
        let receive = publication.is_some();
        let bound = Bound {
            clock: self.clock.as_ref(),
            stopping: Some(stopping),
            runtime: self.runtime.as_ref(),
            hook: &self.hook,
        };
        let remote_path = remote.clone();
        let remote = format!("{}:{remote}", self.target.addr.to_owned());
        // D91: a source on a mount that no longer answers can hold this for ever; a stop ends the wait.
        let source_path = remote_path.clone();
        let cancelled: Arc<dyn Fn() -> Option<Cancellation> + Send + Sync> = Arc::from(cancelled);
        let stage_stop = cancelled.clone();
        let resolved = bound
            .run(
                None,
                None,
                true,
                blocking(self.runtime.as_ref(), {
                    move || {
                        let cwd = std::env::current_dir().map_err(io_error)?;
                        let original = absolute_file_path(&local.to_string_lossy(), &cwd.to_string_lossy());
                        let place = original.to_string_lossy().into_owned();
                        let (path, stage) = if let Some(name) = publication {
                            let bundle = name == ReceiveName::Bundle;
                            let into_directory = local.is_dir() && !source_path.ends_with('/');
                            let source = Path::new(&source_path);
                            let destination = if into_directory && !bundle {
                                original.join(source.file_name().ok_or_else(|| {
                                    VendorError::Other(failure(FailureKind::FileSystem, &source_path))
                                })?)
                            } else {
                                original
                            };
                            let mut stage = crate::cmd::artifact_stage::ArtifactStage::new(destination, bundle)?;
                            let path = if bundle || into_directory {
                                stage.rsync_directory(source)?
                            } else {
                                if source_path.ends_with('/') {
                                    stage.seed_directory(stage_stop.as_ref())?;
                                }
                                stage.content().to_owned()
                            };
                            (path, Some(stage))
                        } else {
                            (original, None)
                        };
                        let mut text = path.to_string_lossy().into_owned();
                        if path.is_dir() {
                            text = format!("{}/", text.trim_end_matches('/'));
                        }
                        Ok((text, place, stage))
                    }
                }),
            )
            .await;
        let (local, publish_place, prepared) = match resolved {
            Step::Done(local) => local?,
            Step::Stopped | Step::Expired => return self.stopped(stopping.stopped(), receive, &remote),
        };
        *stage = prepared;
        if let Some(stopped) = stopping.stopped() {
            return self.stopped(Some(stopped), receive, &remote);
        }
        let arriving = receive.then(|| Arriving::new(&local, &remote_path));
        let (source, destination) = if receive { (remote, local) } else { (local, remote) };
        let place = if receive { publish_place } else { destination.clone() };
        let place = place.trim_end_matches('/').to_owned();
        // D91: an earlier transfer into this destination may still be writing there. Checked again as rsync starts.
        if self
            .admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .quarantined
            .contains(&place)
        {
            return self.quarantined(receive, &destination);
        }
        let outcome = self
            .process(
                self.arguments(source.clone(), destination.clone())?,
                arriving,
                matches!(self.auth, Some(SshAuthentication::Password { .. })),
                stopping,
                Some(Box::new(move || cancelled())),
                Some(place.clone()),
                unconfirmed,
            )
            .await;
        if let Some(why) = unconfirmed.as_ref() {
            // D91: never waits for a start under way; one into this destination is killed at its commit. The lock is let go
            // before the log sink, which is the run's to configure, is called.
            {
                let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
                for (starting, ticket) in &admission.starting {
                    if *starting == place {
                        ticket.refuse(true);
                    }
                }
                admission.quarantined.insert(place);
            }
            if self.logging.enabled(&self.logger, Level::ERROR) {
                let message = format!(
                    "{} error: could not confirm that the transfer {} {} ended, and nothing more is transferred there in this run",
                    if receive { "receive" } else { "send" },
                    if receive { "into" } else { "to" },
                    destination
                );
                (self.logging.sink)(&self.logger, Level::ERROR, message, Some(why.clone()));
            }
        }
        let Outcome {
            status,
            stderr,
            stalled,
            stopped,
            quarantined,
            ..
        } = outcome?;
        let remote = if receive { &source } else { &destination };
        if quarantined {
            return self.quarantined(receive, &destination);
        }
        if stopped.is_some() {
            return self.stopped(stopped, receive, remote);
        }
        // D91: a transfer whose end could not be confirmed is never a success, so that nothing (`SharedTransfers`) keeps
        // it as one: a stop is returned as the stop, whatever rsync's status; otherwise it fails as a stall does.
        if unconfirmed.is_some() && !stalled {
            if let Some(stopped) = stopping.stopped() {
                return self.stopped(Some(stopped), receive, remote);
            }
            if receive {
                return Ok(false);
            }
            return Err(VendorError::LeavePool {
                reason: LeavePoolReason::TransferStalled,
                detail: failure(
                    TRANSFER_STALLED,
                    format!("Could not confirm that the send to {destination} ended, the device leaves its pool"),
                ),
            });
        }
        if stalled || (status != 0 && !stderr.is_empty()) {
            let text = stderr.clone();
            self.logging.log(
                &self.logger,
                Level::ERROR,
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
    /// D91: the outcome of a transfer into `destination`, where an earlier one was not confirmed to have ended, which is not
    /// started. A send fails as a stalled one does (U163): the device leaves its pool, since what it would install there
    /// may be overwritten. A receive is the `false` its listener deals with.
    fn quarantined(&self, receive: bool, destination: &str) -> VendorResult<bool> {
        let message = format!(
            "Not {} {destination}: an earlier transfer there was not confirmed to have ended",
            if receive { "receiving into" } else { "sending to" }
        );
        if receive {
            self.logging
                .log(&self.logger, Level::ERROR, || format!("receive error: {message}"), None);
            return Ok(false);
        }
        self.logging.log(&self.logger, Level::WARN, || message.clone(), None);
        Err(VendorError::LeavePool {
            reason: LeavePoolReason::TransferStalled,
            detail: failure(TRANSFER_STALLED, format!("{message}, the device leaves its pool")),
        })
    }
    /// D54: the outcome of a transfer to or from `remote` that `stopped` ended, or kept from starting. A cancellation is the
    /// caller's to report. A lost connection fails a send, which nothing retries on that device since its commands fail too,
    /// and leaves a receive the `false` its listener deals with, as D43 does.
    fn stopped(&self, stopped: Option<TransferStopped>, receive: bool, remote: &str) -> VendorResult<bool> {
        let stopped =
            stopped.unwrap_or_else(|| TransferStopped::Cancelled(crate::cmd::cancelled("The transfer was stopped")));
        match stopped {
            TransferStopped::Cancelled(reason) => Err(VendorError::Cancelled(reason)),
            TransferStopped::Lost(lost) => {
                if self.logging.enabled(&self.logger, Level::ERROR) {
                    let message = if receive {
                        format!("receive error: the device's connection was lost while receiving {remote}")
                    } else {
                        format!("send error: the device's connection was lost while sending to {remote}")
                    };
                    (self.logging.sink)(&self.logger, Level::ERROR, message, Some(lost.clone()));
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
    /// D91: not started, since its destination was quarantined as it was about to.
    quarantined: bool,
}
impl Outcome {
    /// D91: a process that a stop kept from starting.
    fn not_started(stopped: Option<TransferStopped>) -> Self {
        Self {
            status: -1,
            stdout: String::new(),
            stderr: String::new(),
            stalled: false,
            stopped: Some(
                stopped
                    .unwrap_or_else(|| TransferStopped::Cancelled(crate::cmd::cancelled("The transfer was stopped"))),
            ),
            quarantined: false,
        }
    }
    fn quarantined() -> Self {
        Self {
            quarantined: true,
            stopped: None,
            ..Self::not_started(None)
        }
    }
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
/// D91: whether and why a transfer was stopped, from its caller, its device or the loss of its connection, as one task
/// (`Inner::transfer`) sees it for the transfer's whole life, and when its cleanup must have ended.
struct Stopping {
    stopped: OnceLock<TransferStopped>,
    /// On the vendor clock: the stop and the 40 s after it.
    at: OnceLock<i64>,
    changed: Notify,
    /// The device's hold, which can ask for an earlier end, also while a step is under way.
    owner: Option<Arc<crate::cmd::TransferReceipt>>,
}
impl Stopping {
    fn new(owner: Option<Arc<crate::cmd::TransferReceipt>>) -> Arc<Self> {
        Arc::new(Self {
            stopped: OnceLock::new(),
            at: OnceLock::new(),
            changed: Notify::new(),
            owner,
        })
    }
    fn stop(&self, why: TransferStopped, now: i64) {
        let _ = self.at.set(now.saturating_add(STOP_CLEANUP_MILLIS));
        let _ = self.stopped.set(why);
        self.changed.notify_waiters();
    }
    fn stopped(&self) -> Option<TransferStopped> {
        self.stopped.get().cloned()
    }
    /// When the cleanup must have ended, once there is a stop.
    fn deadline(&self) -> Option<i64> {
        let at = *self.at.get()?;
        Some(
            self.owner
                .as_ref()
                .and_then(|o| o.deadline())
                .map_or(at, |by| by.min(at)),
        )
    }
    async fn wait(&self) {
        loop {
            let changed = self.changed.notified();
            let mut changed = std::pin::pin!(changed);
            changed.as_mut().enable();
            if self.stopped.get().is_some() {
                return;
            }
            changed.await;
        }
    }
}
/// How a step of a transfer ended.
enum Step<T> {
    Done(T),
    /// Its own limit or the stop's deadline passed.
    Expired,
    /// A stop came, and the step was one that a stop ends.
    Stopped,
}
impl<T> Step<T> {
    fn done(self) -> Option<T> {
        match self {
            Self::Done(value) => Some(value),
            _ => None,
        }
    }
}
/// D91: the one way a transfer waits. Each step gets its own limit, in real time or on the vendor clock, and, once the
/// transfer is stopped, what is left until its deadline, read again whenever a stop or an earlier deadline comes, so that
/// one given during a step ends that step. A clock that fails leaves its limits to real time.
#[derive(Clone, Copy)]
struct Bound<'a> {
    clock: &'a dyn VendorClock,
    stopping: Option<&'a Stopping>,
    /// Where `ps` is started, on a blocking thread, so that its start is inside the step's limits too.
    runtime: &'a dyn VendorRuntime,
    hook: &'a HookSlot,
}
impl Bound<'_> {
    fn left(&self) -> Option<i64> {
        let at = self.stopping?.deadline()?;
        Some(at.saturating_sub(self.clock.now_millis()).max(0))
    }
    fn expired(&self) -> bool {
        self.left() == Some(0)
    }
    fn stopped(&self) -> bool {
        self.stopping.is_some_and(|s| s.stopped.get().is_some())
    }
    /// `body` within `real` and `vendor`, and within the deadline once there is a stop. With `interruptible`, a stop ends the
    /// step too. A step with no time left still sees what `body` has ready at once, such as a process already reaped.
    async fn run<T>(
        &self,
        real: Option<std::time::Duration>,
        vendor: Option<i64>,
        interruptible: bool,
        body: impl Future<Output = T>,
    ) -> Step<T> {
        let real_end = real.map(|limit| tokio::time::Instant::now() + limit);
        let own_end = vendor.map(|limit| self.clock.now_millis().saturating_add(limit));
        let mut body = std::pin::pin!(body);
        loop {
            let changed = self.stopping.map(|s| s.changed.notified());
            let mut changed = std::pin::pin!(changed);
            if let Some(changed) = changed.as_mut().as_pin_mut() {
                changed.enable();
            }
            let owner = self.stopping.and_then(|s| s.owner.as_deref()).map(|o| o.changed());
            let mut owner = std::pin::pin!(owner);
            if let Some(owner) = owner.as_mut().as_pin_mut() {
                owner.enable();
            }
            if interruptible && self.stopped() {
                let ready = std::future::poll_fn(|cx| match body.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => std::task::Poll::Ready(Some(value)),
                    std::task::Poll::Pending => std::task::Poll::Ready(None),
                })
                .await;
                return ready.map_or(Step::Stopped, Step::Done);
            }
            let now = self.clock.now_millis();
            let end = match (own_end, self.stopping.and_then(|s| s.deadline())) {
                (Some(own), Some(at)) => Some(own.min(at)),
                (own, at) => own.or(at),
            };
            let vendor_left = end.map(|end| end.saturating_sub(now).max(0));
            let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
            let vendor_limit = async {
                match vendor_left {
                    Some(millis) => {
                        if self.clock.delay(millis, scope).await.is_err() {
                            tokio::time::sleep(std::time::Duration::from_millis(millis as u64)).await;
                        }
                    }
                    None => std::future::pending().await,
                }
            };
            let real_limit = async {
                match real_end {
                    Some(end) => tokio::time::sleep_until(end).await,
                    None => std::future::pending().await,
                }
            };
            let renewed = async {
                match (changed.as_mut().as_pin_mut(), owner.as_mut().as_pin_mut()) {
                    (Some(changed), Some(owner)) => {
                        tokio::select! {
                            _ = changed => {}
                            _ = owner => {}
                        }
                    }
                    (Some(changed), None) => changed.await,
                    (None, Some(owner)) => owner.await,
                    (None, None) => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                value = &mut body => return Step::Done(value),
                _ = vendor_limit => return Step::Expired,
                _ = real_limit => return Step::Expired,
                _ = renewed => {}
            }
        }
    }
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
    async fn list(&self, unreaped: bool, bound: Bound<'_>) -> Option<std::collections::HashMap<i32, (i32, String)>> {
        let listing = processes(bound).await?;
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
    /// and counts as a parent only while `unreaped`. With no listing, nobody. Returns those signalled, in the order sent.
    ///
    /// Each gets it before the processes it started, as the child does before them (x50). Under `sshpass` rsync is one of
    /// these, and an rsync that its ssh's end reached before its own TERM could finish all the same, which counts as success.
    async fn signal(&self, signal: nix::sys::signal::Signal, unreaped: bool, bound: Bound<'_>) -> Vec<i32> {
        let Some(listing) = self.list(unreaped, bound).await else {
            return Vec::new();
        };
        let seen = self.seen();
        let mut ours: std::collections::HashSet<i32> = std::collections::HashSet::new();
        if unreaped {
            ours.insert(self.root);
        }
        // Found a generation at a time, so a process comes after its parent.
        let mut order = Vec::new();
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
            ours.extend(&found);
            order.extend(found);
        }
        drop(seen);
        for pid in &order {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid), signal);
        }
        order
    }
    /// After a stall or a stop, with the child reaped, or not if `unreaped`, which the caller has sent KILL: TERM to what
    /// it started, 5 s for them to go, then KILL to any still there. D91: then which of those seen are still running,
    /// looked at for up to 5 s more (`REAP_LIMIT`), or `None` if no listing could tell; all of it within `bound`.
    async fn end(&self, unreaped: bool, bound: Bound<'_>) -> Option<Vec<i32>> {
        use nix::sys::signal::{Signal, kill};
        let clock = bound.clock;
        let scope: Arc<dyn CancellationSignal> = Arc::new(SessionScope::default());
        let termed = self.signal(Signal::SIGTERM, unreaped, bound).await;
        if !termed.is_empty() {
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
            if bound
                .run(None, Some(STOP_STEP_MILLIS), false, gone)
                .await
                .done()
                .is_none()
            {
                self.signal(Signal::SIGKILL, unreaped, bound).await;
            }
        }
        // A killed orphan is listed until init has reaped it, so the look is repeated for a while.
        let left = Mutex::new(None);
        let gone = async {
            loop {
                let Some(listing) = processes(bound).await else {
                    return;
                };
                let running: Vec<i32> = self
                    .seen()
                    .iter()
                    .filter(|(pid, started)| {
                        **pid != self.root && listing.get(pid).is_some_and(|(_, now)| now == *started)
                    })
                    .map(|(pid, _)| *pid)
                    .collect();
                let done = running.is_empty();
                *left.lock().unwrap_or_else(|e| e.into_inner()) = Some(running);
                if done || clock.delay(STOP_POLL_MILLIS, scope.clone()).await.is_err() {
                    return;
                }
            }
        };
        // Real time, as the reap: when init gets to them is the kernel's, however fast the run's clock goes.
        bound.run(Some(REAP_LIMIT), None, false, gone).await;
        let mut left = left.into_inner().unwrap_or_else(|e| e.into_inner());
        if let Some(left) = &mut left {
            left.sort_unstable();
        }
        left
    }
    /// Lists while the transfer runs; never ends.
    async fn follow(&self, bound: Bound<'_>) {
        tokio::time::sleep(LIST_FIRST).await;
        loop {
            self.list(true, bound).await;
            tokio::time::sleep(LIST_EVERY).await;
        }
    }
}
/// `ps -A -o pid=,ppid=,lstart=`: every process by pid, with its parent and when it started, or `None` if `ps` fails or does
/// not answer within `PROCESS_LIMIT` and `bound`.
async fn processes(bound: Bound<'_>) -> Option<std::collections::HashMap<i32, (i32, String)>> {
    if bound.expired() {
        return None;
    }
    // D91: started on a blocking thread, since starting a process looks it up on `PATH` and can hang on a mount that no
    // longer answers; the limits cover the start too. A `ps` that is given up is killed when it is dropped.
    let hook = Hook::of(bound.hook);
    let listing = async move {
        let child = blocking(bound.runtime, move || {
            hook.call(BlockingCall::SpawnPs);
            Command::new("ps")
                .args(["-A", "-o", "pid=,ppid=,lstart="])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(io_error)
        })
        .await
        .ok()?;
        child.wait_with_output().await.ok()
    };
    let output = bound.run(Some(PROCESS_LIMIT), None, false, listing).await.done()??;
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
/// D31, D43: whether a transfer that failed with `status`, `quiet` ms after the last progress the bridge saw, stalled:
/// the bridge saw nothing arrive for the limit, or rsync reported a stall after `STALL_AFTER_MILLIS`. On a receive from
/// an openrsync worker, rsync 3.4 and 3.5 report from the generator, exit 30 and add the receiver's line `received SIGUSR1
/// (code 19)`.
fn is_stall(status: i32, stderr: &str, quiet: i64, nothing_arrived: bool) -> bool {
    status != 0
        && (nothing_arrived
            || (quiet >= STALL_AFTER_MILLIS
                && (reports_stall(stderr.as_bytes())
                    || (status == RSYNC_TIMEOUT_STATUS && !stderr.contains(STOPPED_AT_LIMIT)))))
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
            // D91: a stop comes first, so a pipe that something keeps full cannot hold a reader that has been told to end.
            let count = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                result = pipe.read(&mut chunk) => result.map_err(io_error)?,
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
    let text = String::from_utf8_lossy(&bytes)
        .into_owned()
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
                        let remote = format!("{}:{dst}", inner.target.addr.to_owned());
                        return inner.stopped(Some(stopped), false, &remote);
                    }
                };
                // U163: this send waited out another's 300 s to the same worker, and would most likely spend as long itself.
                if let Some(error) = inner.stalls.since(arrived) {
                    inner.logging.log(
                        &inner.logger,
                        Level::WARN,
                        || format!("Not sending to {dst}: a send to this worker stalled while this one waited"),
                        None,
                    );
                    return Err(error);
                }
                inner.transfer(src, dst, None, stop).await
            })
            .await
        })
    }
    fn receive_with<'a>(&'a self, src: &'a str, dst: &'a Path, stop: TransferStop) -> VendorFuture<'a, bool> {
        self.receive_staged(src, dst, stop, ReceiveName::Source)
    }
    fn receive_bundle<'a>(&'a self, src: &'a str, dst: &'a Path, stop: TransferStop) -> VendorFuture<'a, bool> {
        self.receive_staged(src, dst, stop, ReceiveName::Bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock whose delays never end, for steps with only real-time limits.
    struct NoClock;
    impl VendorClock for NoClock {
        fn delay(&self, _: i64, _: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
        fn with_timeout<'a>(
            &'a self,
            _: Arc<dyn CancellationSignal>,
            _: i64,
            _: marathon_core::runtime::VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, ()> {
            Box::pin(std::future::pending())
        }
        fn with_timeout_or_null<'a>(
            &'a self,
            _: Arc<dyn CancellationSignal>,
            _: i64,
            _: marathon_core::runtime::VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, Option<()>> {
            Box::pin(std::future::pending())
        }
    }
    /// Runs blocking work on tokio's blocking threads.
    struct Threads;
    impl VendorRuntime for Threads {
        fn spawn_cancellable(
            &self,
            body: marathon_core::runtime::VendorTaskBody,
        ) -> Arc<dyn marathon_core::runtime::VendorTask> {
            self.spawn(body(Arc::new(SessionScope::default())))
        }
        fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn marathon_core::runtime::VendorTask> {
            Arc::new(Task(std::sync::Mutex::new(Some(tokio::spawn(body)))))
        }
        fn spawn_blocking(
            &self,
            body: Box<dyn FnOnce() -> VendorResult<()> + Send>,
        ) -> Arc<dyn marathon_core::runtime::VendorTask> {
            Arc::new(Task(std::sync::Mutex::new(Some(tokio::task::spawn_blocking(body)))))
        }
    }
    struct Task(std::sync::Mutex<Option<tokio::task::JoinHandle<VendorResult<()>>>>);
    impl marathon_core::runtime::VendorTask for Task {
        fn cancel(&self, _: Cancellation) {}
        fn join(&self) -> VendorFuture<'_, ()> {
            let handle = self.0.lock().unwrap().take();
            Box::pin(async move {
                match handle {
                    Some(handle) => handle.await.unwrap(),
                    None => Ok(()),
                }
            })
        }
    }
    #[cfg(feature = "test-support")]
    static NO_HOOK: HookSlot = OnceLock::new();
    #[cfg(not(feature = "test-support"))]
    static NO_HOOK: HookSlot = ();
    const UNBOUNDED: Bound<'static> = Bound {
        clock: &NoClock,
        stopping: None,
        runtime: &Threads,
        hook: &NO_HOOK,
    };

    #[tokio::test(flavor = "multi_thread")]
    async fn processes_tell_a_process_from_one_that_is_gone() {
        let own = std::process::id() as i32;
        let listing = processes(UNBOUNDED).await.unwrap();
        let (parent, started) = listing.get(&own).unwrap().clone();
        assert_eq!(parent, nix::unistd::getppid().as_raw());
        assert_eq!(processes(UNBOUNDED).await.unwrap().get(&own), Some(&(parent, started)));
        let mut child = Command::new("/bin/sleep").arg("100").spawn().unwrap();
        let pid = child.id().unwrap() as i32;
        assert_eq!(
            processes(UNBOUNDED).await.unwrap().get(&pid).map(|(parent, _)| *parent),
            Some(own)
        );
        child.kill().await.unwrap();
        assert_eq!(processes(UNBOUNDED).await.unwrap().get(&pid), None);
    }

    /// x50: a stop's signal reaches each process before the ones it started. Sent in no order, a TERM to ssh could come
    /// first, and an rsync under `sshpass` that saw its ssh end before its own TERM could exit 0, which counts as success:
    /// GitHub's ubuntu-22.04 runner did that once. A chain of five below the child, so that no order but the right one passes
    /// (the old one did 1 time in 120). SIGCONT, so that nothing ends before the order is read.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stop_signals_each_process_before_the_ones_it_started_u181() {
        use nix::{sys::signal, unistd::Pid};
        let dir = tempfile::tempdir().unwrap();
        let chain = dir.path().join("chain");
        std::fs::write(
            &chain,
            "#!/bin/sh\nif [ \"$1\" -gt 0 ]; then \"$0\" $(($1 - 1)) & wait; else exec /bin/sleep 100041; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&chain, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let mut child = Command::new(&chain).arg("5").spawn().unwrap();
        let root = child.id().unwrap() as i32;
        let descendants = Descendants::of(&child);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while descendants.seen().len() < 6 {
                descendants.list(true, UNBOUNDED).await;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let listing = processes(UNBOUNDED).await.unwrap();
        let order = descendants.signal(signal::Signal::SIGCONT, true, UNBOUNDED).await;
        for pid in order.iter().rev() {
            let _ = signal::kill(Pid::from_raw(*pid), signal::Signal::SIGKILL);
        }
        child.kill().await.unwrap();
        assert_eq!(order.len(), 5, "{order:?}");
        let mut parent = root;
        for pid in &order {
            assert_eq!(listing.get(pid).map(|(parent, _)| *parent), Some(parent), "{order:?}");
            parent = *pid;
        }
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

    /// x50: what a stalled pull from rsync 3.5.2 to an openrsync worker printed, as measured, and its status 30. The 19 is
    /// the receiver child's line, not the status, but a 19 with the report is a stall too. Never before the limit: then it is
    /// a quick failure.
    #[test]
    fn rsync_3_5s_report_from_the_generator_is_a_stall_only_after_the_limit_d43() {
        let said = "Warning: Permanently added '[127.0.0.1]:62259' (ED25519) to the list of known hosts.\n\
            [generator] io timeout after 300 seconds -- exiting\n\
            rsync error: timeout in data send/receive (code 30) at io.c(266) [generator=3.5.2]\n\
            rsync error: received SIGUSR1 (code 19) at main.c(1691) [receiver=3.5.2]\n";
        // The same with rsync 3.4.1 and an openrsync worker.
        let said_by_3_4 = "[generator] io timeout after 300 seconds -- exiting\n\
            rsync error: timeout in data send/receive (code 30) at io.c(201) [generator=3.4.1]\n\
            rsync error: received SIGUSR1 (code 19) at main.c(1600) [receiver=3.4.1]\n";
        for stderr in [said, said_by_3_4] {
            for status in [30, 19] {
                assert!(
                    is_stall(status, stderr, STALL_AFTER_MILLIS, false),
                    "{status} {stderr:?}"
                );
                assert!(is_stall(status, stderr, 450_000, false), "{status} {stderr:?}");
                assert!(
                    !is_stall(status, stderr, STALL_AFTER_MILLIS - 1, false),
                    "{status} {stderr:?}"
                );
                assert!(!is_stall(status, stderr, 40_000, false), "{status} {stderr:?}");
            }
            // A status of 0 is never a stall, whatever was said.
            assert!(!is_stall(0, stderr, 450_000, true));
        }
        // Nothing arriving for the limit is a stall whatever rsync said, and 30 alone is one after the limit.
        assert!(is_stall(143, "", 0, true));
        assert!(is_stall(30, "", STALL_AFTER_MILLIS, false));
        assert!(!is_stall(30, "", STALL_AFTER_MILLIS - 1, false));
        // The receiver's line alone is not a report.
        let receiver = "rsync error: received SIGUSR1 (code 19) at main.c(1691) [receiver=3.5.2]\n";
        assert!(!is_stall(19, receiver, 450_000, false));
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
