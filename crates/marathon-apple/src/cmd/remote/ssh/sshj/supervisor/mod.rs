//! D96 worker lease and process-group receipts.

use super::{channel::Stream, client::SshConnection, config::SshLogging};
use crate::cmd::{TransferCleanup, TransferReceipt, failure};
use base64::{Engine, engine::general_purpose::STANDARD};
use jcompat::jtime::Duration;
use marathon_core::error::{Failure, VendorError, VendorResult};
use marathon_error::FailureKind;
use marathon_logging::Level;
use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration as WallDuration,
};
use tokio::{
    io::AsyncReadExt,
    sync::{Notify, OnceCell, watch},
};

const TARGET: &str = "marathon_apple::cmd::remote::ssh::sshj::supervisor";
const PROTOCOL: &str = "MARATHON_D96_1";
const PROBE_TIMEOUT: WallDuration = WallDuration::from_secs(12);
const CONTROL_TIMEOUT: WallDuration = WallDuration::from_secs(4);
pub(super) const EXIT_PROOF_TIMEOUT: WallDuration = WallDuration::from_secs(24);
const RECONNECT_DELAY: WallDuration = WallDuration::from_secs(1);
const CLOSE_GRACE: WallDuration = WallDuration::from_secs(20);
const BOOTSTRAP: &str = include_str!("bootstrap.pl");
const WORKER: &str = include_str!("worker.pl");

pub type Warnings = Arc<Mutex<HashSet<(String, u16)>>>;
type ControlConnection = Pin<Box<dyn Future<Output = Result<Arc<SshConnection>, super::error::SshFailure>> + Send>>;
pub(super) type ControlDial = Arc<dyn Fn() -> ControlConnection + Send + Sync>;

#[derive(Clone, Copy)]
struct Timing {
    interval: WallDuration,
    lease_ms: u64,
}

impl Timing {
    fn from_keep_alive(interval: Duration, count: i32) -> Self {
        let seconds = match interval.seconds() {
            n if n > 0 => n.clamp(1, 60) as u64,
            _ => 15,
        };
        let misses = count.clamp(1, 20) as u64;
        Self {
            interval: WallDuration::from_secs(seconds),
            // A renewal may be observed one exchange late. Budget also for
            // a wait on a shared dial, the failed exchange, the reconnect,
            // an exchange on the new connection, and a data-path exchange.
            lease_ms: 1000
                * (seconds * (misses + 1) + CONTROL_TIMEOUT.as_secs())
                    .max(seconds + 6 * CONTROL_TIMEOUT.as_secs() + 2 * RECONNECT_DELAY.as_secs()),
        }
    }
}

pub(super) struct Supervisor {
    capability: OnceCell<Option<String>>,
    peer: OnceCell<Arc<Peer>>,
    dial: ControlDial,
    worker: (String, u16),
    warnings: Warnings,
    logging: SshLogging,
    timing: Timing,
}

impl Supervisor {
    pub(super) fn new(
        worker: (String, u16),
        warnings: Warnings,
        logging: SshLogging,
        interval: Duration,
        count: i32,
        dial: ControlDial,
    ) -> Self {
        Self {
            capability: OnceCell::new(),
            peer: OnceCell::new(),
            dial,
            worker,
            warnings,
            logging,
            timing: Timing::from_keep_alive(interval, count),
        }
    }

    pub(super) async fn available(&self, connection: &Arc<SshConnection>) -> VendorResult<bool> {
        Ok(self.peer(connection).await?.is_some())
    }

    pub(super) async fn prepare(
        &self,
        connection: &Arc<SshConnection>,
        command: &str,
        detached: bool,
    ) -> VendorResult<Option<Arc<Lease>>> {
        // An absolute command made solely of shell literals needs no extra
        // shell. All other commands retain the executor's shell semantics.
        let argv = if command.starts_with('/')
            && !command.starts_with("/bin/sh -c ")
            && command
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b" /._-".contains(&byte))
        {
            command.to_owned()
        } else {
            format!("/bin/sh -c {}", quote(command))
        };
        Ok(self
            .peer(connection)
            .await?
            .map(|peer| Arc::new(Lease::new(peer, if detached { "detach" } else { "run" }, argv))))
    }

    pub(super) async fn prepare_rsync(
        &self,
        connection: &Arc<SshConnection>,
        program: &str,
        auth: marathon_config::vendor::SshAuthentication,
    ) -> VendorResult<Option<WorkerRsync>> {
        Ok(self.peer(connection).await?.map(|peer| WorkerRsync {
            // openrsync rebuilds the remote command from argv and removes
            // shell quotes. Keep the envelope free of shell syntax.
            lease: Arc::new(Lease::new(peer, "rsync", STANDARD.encode(program))),
            auth,
            started: false,
        }))
    }

    pub(super) async fn retire(&self) {
        let Some(peer) = self.peer.get() else {
            return;
        };
        let result = tokio::time::timeout(CONTROL_TIMEOUT, peer.retire()).await;
        if !matches!(result, Ok(Ok(()))) {
            let logger = self.logging.component(TARGET);
            self.logging.log(
                &logger,
                Level::DEBUG,
                || "SSH worker supervisor state could not be retired on close".into(),
                None,
            );
        }
    }

    async fn peer(&self, connection: &Arc<SshConnection>) -> VendorResult<Option<Arc<Peer>>> {
        let capability = self.capability.get_or_try_init(|| async {
            let install = format!(
                "/usr/bin/perl -MMIME::Base64 -e 'eval decode_base64 shift; die $@ if $@' -- {} {}",
                STANDARD.encode(BOOTSTRAP),
                STANDARD.encode(WORKER)
            );
            let (status, output) = exchange(connection, install, PROBE_TIMEOUT)
                .await
                .map_err(ExchangeError::into_vendor)?;
            if status != 0 {
                let first = self.warnings.lock().unwrap_or_else(|e| e.into_inner()).insert(self.worker.clone());
                if first {
                    let logger = self.logging.component(TARGET);
                    self.logging.log(&logger, Level::WARN, || {
                        format!("SSH worker {}:{} has no usable /usr/bin/perl with the required core modules. Leftover worker processes are not contained.", self.worker.0, self.worker.1)
                    }, None);
                }
                return Ok(None);
            }
            let root = output.trim().strip_prefix(&format!("{PROTOCOL} ")).filter(|root| valid_root(root));
            let Some(root) = root.filter(|_| status == 0) else {
                return Err(protocol_error("SSH worker returned an invalid supervisor bootstrap response"));
            };
            Ok(Some(root.to_owned()))
        }).await?;
        let Some(root) = capability.clone() else {
            return Ok(None);
        };
        let peer = self
            .peer
            .get_or_init(|| async {
                Arc::new(Peer {
                    data: connection.clone(),
                    dial: self.dial.clone(),
                    control: tokio::sync::Mutex::default(),
                    root,
                    timing: self.timing,
                })
            })
            .await;
        Ok(Some(peer.clone()))
    }
}

fn valid_root(value: &str) -> bool {
    value.strip_prefix("/tmp/marathon-supervisor-").is_some_and(|suffix| {
        suffix.len() == 10
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    })
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn protocol_error(message: &str) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, message))
}

enum ExchangeError {
    Timeout(&'static str),
    Command(VendorError),
}
impl ExchangeError {
    fn into_vendor(self) -> VendorError {
        match self {
            Self::Timeout(message) => protocol_error(message),
            Self::Command(error) => error,
        }
    }
}

async fn exchange(
    connection: &SshConnection,
    wire: String,
    timeout: WallDuration,
) -> Result<(i32, String), ExchangeError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let channel = tokio::time::timeout_at(deadline, connection.start_session())
        .await
        .map_err(|_| ExchangeError::Timeout("Worker supervisor channel open timed out"))?
        .map_err(|error| ExchangeError::Command(error.into_vendor()))?;
    let result = tokio::time::timeout_at(deadline, async {
        channel
            .exec(wire)
            .await
            .map_err(super::error::SshFailure::into_vendor)?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = channel.reader(Stream::Stdout).take(32 * 1024 + 1);
        let mut stderr = channel.reader(Stream::Stderr).take(32 * 1024 + 1);
        tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err)).map_err(crate::cmd::io_error)?;
        if out.len() > 32 * 1024 || err.len() > 32 * 1024 {
            return Err(protocol_error("Worker supervisor response exceeded its limit"));
        }
        let code = channel
            .protocol_exit()
            .await
            .ok_or_else(|| protocol_error("Worker supervisor exited without a status"))?;
        let output = String::from_utf8(out).map_err(|_| protocol_error("Worker supervisor response was not UTF-8"))?;
        Ok((code, output))
    })
    .await;
    channel.request_close();
    result
        .map_err(|_| ExchangeError::Timeout("Worker supervisor command timed out"))?
        .map_err(ExchangeError::Command)
}

#[derive(Default)]
struct Control {
    active: Option<Arc<SshConnection>>,
    next_dial: Option<tokio::time::Instant>,
    retry_delay: Option<WallDuration>,
}
struct Peer {
    data: Arc<SshConnection>,
    dial: ControlDial,
    control: tokio::sync::Mutex<Control>,
    root: String,
    timing: Timing,
}

impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(active) = self.control.get_mut().active.take() {
            active.abandon();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Proof {
    Done(i32),
    NotStarted,
}

#[derive(Debug, Eq, PartialEq)]
enum Reply {
    Running(u64),
    Busy,
    Unknown,
    Proof(Proof),
    Unconfirmed,
}

impl Reply {
    fn parse(output: &str, token: &str) -> Option<Self> {
        let (returned, state) = output.strip_suffix('\n')?.split_once(' ')?;
        if returned != token {
            return None;
        }
        Some(match state {
            "busy" => Self::Busy,
            "unknown" => Self::Unknown,
            "not-started" => Self::Proof(Proof::NotStarted),
            "unconfirmed" => Self::Unconfirmed,
            _ if state.starts_with("running:") => Self::Running(state[8..].parse().ok()?),
            _ if state.starts_with("done:") => Self::Proof(Proof::Done(i32::from(state[5..].parse::<u8>().ok()?))),
            _ => return None,
        })
    }
}

impl Peer {
    async fn retire(&self) -> VendorResult<()> {
        let connection = if self.data.is_connected() {
            self.data.clone()
        } else {
            self.control
                .lock()
                .await
                .active
                .clone()
                .filter(|active| active.is_connected())
                .ok_or_else(|| protocol_error("Worker supervisor has no remaining connection"))?
        };
        let wire = format!(
            "/usr/bin/perl {}/supervisor.pl retire {} {}",
            self.root, self.root, self.timing.lease_ms
        );
        loop {
            let (status, output) = exchange(&connection, wire.clone(), CONTROL_TIMEOUT)
                .await
                .map_err(ExchangeError::into_vendor)?;
            match (status, output.as_str()) {
                (0, "MARATHON_D96_1 retired\n") => return Ok(()),
                (0, "MARATHON_D96_1 busy\n") => tokio::time::sleep(WallDuration::from_millis(100)).await,
                _ => return Err(protocol_error("Worker supervisor retirement failed")),
            }
        }
    }

    async fn connection(&self) -> VendorResult<Arc<SshConnection>> {
        let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
        let mut control = tokio::time::timeout_at(deadline, self.control.lock())
            .await
            .map_err(|_| protocol_error("Worker supervisor reconnect is busy"))?;
        if let Some(active) = &control.active
            && active.is_connected()
        {
            return Ok(active.clone());
        }
        if control.next_dial.is_some_and(|next| tokio::time::Instant::now() < next) {
            return Err(protocol_error("Worker supervisor reconnect is waiting to retry"));
        }
        // One dial per shared connection. Its deadline also bounds callers
        // waiting behind it. Keep the failed-attempt bound on cancellation.
        let retry = control.retry_delay.unwrap_or(RECONNECT_DELAY);
        control.next_dial = Some(deadline + retry);
        control.retry_delay = Some((retry * 2).min(WallDuration::from_secs(60)));
        let connected = tokio::time::timeout_at(deadline, (self.dial)()).await;
        control.next_dial = Some(tokio::time::Instant::now() + retry);
        let active = connected
            .map_err(|_| protocol_error("Worker supervisor control connection timed out"))?
            .map_err(super::error::SshFailure::into_vendor)?;
        control.active = Some(active.clone());
        Ok(active)
    }

    async fn control_lost(&self) {
        let active = self
            .control
            .try_lock()
            .ok()
            .and_then(|control| control.active.as_ref().filter(|active| active.is_connected()).cloned());
        match active {
            Some(active) => active.shared.wait_dead().await,
            None => std::future::pending().await,
        }
    }

    async fn exchange_control(
        &self,
        active: &Arc<SshConnection>,
        wire: String,
    ) -> Result<(i32, String), ExchangeError> {
        let result = exchange(active, wire, CONTROL_TIMEOUT).await;
        if result.is_ok()
            && let Ok(mut control) = self.control.try_lock()
            && control
                .active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, active))
        {
            control.retry_delay = None;
            control.next_dial = None;
        }
        result
    }

    async fn ask(&self, token: &str, operation: &str) -> VendorResult<(Reply, bool)> {
        let wire = format!(
            "/usr/bin/perl {}/supervisor.pl control {} {} {token}:{operation}",
            self.root, self.root, self.timing.lease_ms
        );
        let control = async {
            let active = self.connection().await?;
            match self.exchange_control(&active, wire.clone()).await {
                Ok(response) => Ok(response),
                Err(error) if matches!(error, ExchangeError::Timeout(_)) || !active.is_connected() => {
                    // Replace only the generation this request observed. Never
                    // abandon the data transport or a newer control connection.
                    active.abandon();
                    let retry = self.connection().await?;
                    self.exchange_control(&retry, wire.clone())
                        .await
                        .map_err(ExchangeError::into_vendor)
                }
                Err(error) => Err(error.into_vendor()),
            }
        }
        .await;
        let ((status, output), fallback) = match control {
            Ok(response) => (response, false),
            Err(_) if self.data.is_connected() => (
                exchange(&self.data, wire, CONTROL_TIMEOUT)
                    .await
                    .map_err(ExchangeError::into_vendor)?,
                true,
            ),
            Err(error) => return Err(error),
        };
        Reply::parse(&output, token)
            .filter(|_| status == 0)
            .map(|reply| (reply, fallback))
            .ok_or_else(|| protocol_error("Worker supervisor returned an invalid token receipt"))
    }
}

/// The remote sender's token and the credentials under which its state exists.
#[doc(hidden)]
pub struct WorkerRsync {
    lease: Arc<Lease>,
    pub(crate) auth: marathon_config::vendor::SshAuthentication,
    pub(crate) started: bool,
}

impl WorkerRsync {
    pub(crate) const EXIT_PROOF_TIMEOUT: WallDuration = EXIT_PROOF_TIMEOUT;

    pub(crate) fn wire(&self) -> &str {
        self.lease.wire()
    }

    pub(crate) fn start(&mut self) {
        self.started = true;
        // The separate ssh client exposes no exec acknowledgement. A status
        // response from the worker is needed before any renewal.
        self.lease.start(None);
        self.lease.acknowledge();
    }

    pub(crate) fn stop(&self) {
        self.lease.channel_ended();
    }

    pub(crate) fn finish(&self) {
        self.lease.stop.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
        self.lease.changed.notify_one();
    }

    pub(crate) async fn confirmed(&self, successful: bool) -> VendorResult<()> {
        match self.lease.proof().await? {
            Proof::Done(0) => Ok(()),
            _ if !successful => Ok(()),
            _ => Err(protocol_error(
                "Successful rsync has no successful worker sender receipt",
            )),
        }
    }
}

impl Drop for WorkerRsync {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, Default)]
struct Stop {
    signal: u8,
    channel_ended: bool,
    acknowledged: bool,
    finished: bool,
}

pub(super) struct Lease {
    peer: Arc<Peer>,
    token: String,
    wire: String,
    foreground: bool,
    detached: bool,
    stop: Mutex<Stop>,
    changed: Notify,
    proof: watch::Sender<Option<Result<Proof, Failure>>>,
}

impl Lease {
    fn new(peer: Arc<Peer>, mode: &str, argv: String) -> Self {
        let token = format!("{:032x}", rand::random::<u128>());
        let foreground = mode == "run";
        let detached = mode == "detach";
        let prefix = if foreground || detached { "exec " } else { "" };
        let wire = format!(
            "{prefix}/usr/bin/perl {}/supervisor.pl {mode} {} {} {token} {argv}",
            peer.root, peer.root, peer.timing.lease_ms
        );
        Self {
            peer,
            token,
            wire,
            foreground,
            detached,
            stop: Mutex::default(),
            changed: Notify::new(),
            proof: watch::channel(None).0,
        }
    }
    pub(super) fn wire(&self) -> &str {
        &self.wire
    }

    pub(super) fn start(self: &Arc<Self>, receipt: Option<Arc<TransferReceipt>>) {
        let lease = self.clone();
        self.peer.data.runtime.spawn(Box::pin(async move {
            let outcome = tokio::select! {
                biased;
                result = lease.proof() => result,
                result = lease.watch() => result,
            }
            .map_err(|error| error.failure().clone());
            if let Some(receipt) = receipt {
                receipt.end(match &outcome {
                    Ok(_) => TransferCleanup::Confirmed,
                    Err(why) => TransferCleanup::Unconfirmed(why.clone()),
                });
            }
            lease.proof.send_if_modified(|current| {
                if current.is_some() {
                    return false;
                }
                *current = Some(outcome);
                true
            });
            Ok(())
        }));
    }

    pub(super) fn signal(&self, signal: &russh::Sig) {
        let rank = match signal {
            russh::Sig::INT => 1,
            russh::Sig::KILL => 3,
            _ => 2,
        };
        let mut state = self.stop.lock().unwrap_or_else(|error| error.into_inner());
        state.signal = state.signal.max(rank);
        self.changed.notify_one();
    }

    pub(super) fn is_detached(&self) -> bool {
        self.detached
    }

    pub(super) fn acknowledge(&self) {
        if !self.detached {
            self.stop.lock().unwrap_or_else(|error| error.into_inner()).acknowledged = true;
        }
    }

    pub(super) fn data_status(&self, status: i32) {
        if self.detached && status == 0 {
            self.stop.lock().unwrap_or_else(|error| error.into_inner()).acknowledged = true;
        }
        if !self.foreground {
            return;
        }
        // The wrapper reports a normal status only after it proves cleanup.
        // A channel close, or the wrapper's failure signal, is never evidence.
        self.proof.send_if_modified(|proof| {
            if proof.is_some() {
                return false;
            }
            *proof = Some(Ok(Proof::Done(status)));
            true
        });
    }

    pub(super) fn channel_ended(&self) {
        let mut stop = self.stop.lock().unwrap_or_else(|error| error.into_inner());
        if !self.detached || !stop.acknowledged {
            stop.channel_ended = true;
            self.changed.notify_one();
        }
    }

    pub(super) async fn proof(&self) -> VendorResult<Proof> {
        let mut proof = self.proof.subscribe();
        let result = proof
            .wait_for(Option::is_some)
            .await
            .map_err(|_| protocol_error("Worker exit proof was lost"))?;
        match result.as_ref() {
            Some(Ok(proof)) => Ok(*proof),
            Some(Err(error)) => Err(VendorError::Other(error.clone())),
            None => Err(protocol_error("Worker exit proof was lost")),
        }
    }

    async fn watch(&self) -> VendorResult<Proof> {
        let mut stamp = None;
        let mut ending = None;
        let mut next_renewal = tokio::time::Instant::now();
        let mut signalled = 0;
        let mut delay = self.peer.timing.interval;
        loop {
            tokio::select! {
                _ = self.changed.notified() => {},
                _ = self.peer.control_lost() => {},
                _ = self.peer.data.shared.wait_dead(), if self.detached && self.peer.data.is_connected() => {},
                _ = tokio::time::sleep(delay) => {},
            }
            let state = *self.stop.lock().unwrap_or_else(|error| error.into_inner());
            let now = tokio::time::Instant::now();
            let disconnected = state.channel_ended || (self.detached && !self.peer.data.is_connected());
            let finalizing = self.detached && state.signal == 1 && !disconnected;
            if !finalizing && (state.signal != 0 || disconnected || state.finished) && ending.is_none() {
                ending = Some(now);
            }
            let grace = if state.finished {
                WallDuration::from_secs(2)
            } else {
                CLOSE_GRACE
            };
            let wanted = if disconnected || ending.is_some_and(|start| now - start >= grace) {
                3
            } else {
                state.signal
            };
            let operation = if wanted > signalled {
                wanted.to_string()
            } else if (wanted == 0 || (finalizing && now >= next_renewal))
                && !state.finished
                && state.acknowledged
                && let Some(stamp) = stamp
            {
                format!("renew-{stamp}")
            } else {
                "status".to_string()
            };
            match self.peer.ask(&self.token, &operation).await {
                Ok((Reply::Proof(proof), _)) => return Ok(proof),
                Ok((Reply::Unconfirmed, _)) => {
                    return Err(protocol_error("Worker process group disappearance was not confirmed"));
                }
                Ok((Reply::Running(expiry), _)) => {
                    signalled = wanted;
                    if operation.starts_with("renew-") {
                        next_renewal = now + self.peer.timing.interval;
                    }
                    let needs_renewal = stamp.is_none() && ending.is_none();
                    stamp = Some(expiry);
                    delay = if needs_renewal {
                        WallDuration::ZERO
                    } else if ending.is_some() || finalizing {
                        WallDuration::from_millis(100)
                    } else {
                        self.peer.timing.interval
                    };
                }
                Ok((Reply::Busy | Reply::Unknown, _)) => delay = WallDuration::from_millis(100),
                Err(_) => delay = WallDuration::from_secs(1),
            }
            if ending.is_some_and(|start| now - start >= CLOSE_GRACE + 3 * CONTROL_TIMEOUT) {
                return Err(protocol_error("Worker exit proof did not arrive during cleanup"));
            }
        }
    }
}
