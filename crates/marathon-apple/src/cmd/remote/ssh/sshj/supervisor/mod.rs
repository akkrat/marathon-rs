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
const CONTROL_TIMEOUT: WallDuration = WallDuration::from_secs(4);
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
            lease_ms: 1000 * (seconds * (misses + 1) + CONTROL_TIMEOUT.as_secs()),
        }
    }
}

pub(super) struct Supervisor {
    capability: OnceCell<VendorResult<Option<String>>>,
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

    pub(super) fn connected(&self) -> bool {
        self.peer.get().is_none_or(|peer| peer.connection.is_connected())
    }

    pub(super) async fn available(&self, connection: &Arc<SshConnection>) -> VendorResult<bool> {
        Ok(self.peer(connection).await?.is_some())
    }

    pub(super) async fn prepare(
        &self,
        connection: &Arc<SshConnection>,
        command: &str,
    ) -> VendorResult<Option<Arc<Lease>>> {
        Ok(self
            .peer(connection)
            .await?
            .map(|peer| Arc::new(Lease::new(peer, command))))
    }

    async fn peer(&self, connection: &Arc<SshConnection>) -> VendorResult<Option<Arc<Peer>>> {
        let capability = self.capability.get_or_init(|| async {
            let install = format!(
                "/usr/bin/perl -e {} -- {}",
                quote(BOOTSTRAP),
                STANDARD.encode(WORKER)
            );
            let (status, output) = exchange(connection, install).await?;
            if matches!(status, 2 | 126 | 127) {
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
        }).await;
        let Some(root) = capability.clone()? else {
            return Ok(None);
        };
        let peer = tokio::time::timeout(
            CONTROL_TIMEOUT,
            self.peer.get_or_try_init(|| async {
                let control = (self.dial)().await.map_err(super::error::SshFailure::into_vendor)?;
                Ok(Arc::new(Peer {
                    connection: control,
                    root,
                    timing: self.timing,
                }))
            }),
        )
        .await
        .map_err(|_| protocol_error("Worker supervisor control connection timed out"))??;
        if !peer.connection.is_connected() {
            return Err(protocol_error("Worker supervisor control connection is closed"));
        }
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

async fn exchange(connection: &SshConnection, wire: String) -> VendorResult<(i32, String)> {
    let deadline = tokio::time::Instant::now() + CONTROL_TIMEOUT;
    let channel = tokio::time::timeout_at(deadline, connection.start_session())
        .await
        .map_err(|_| protocol_error("Worker supervisor channel open timed out"))?
        .map_err(super::error::SshFailure::into_vendor)?;
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
    result.map_err(|_| protocol_error("Worker supervisor command timed out"))?
}

struct Peer {
    connection: Arc<SshConnection>,
    root: String,
    timing: Timing,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.connection.abandon();
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
    async fn ask(&self, token: &str, operation: &str) -> VendorResult<Reply> {
        let wire = format!(
            "/usr/bin/perl {}/supervisor.pl control {} {} {token}:{operation}",
            self.root, self.root, self.timing.lease_ms
        );
        let (status, output) = exchange(&self.connection, wire).await?;
        Reply::parse(&output, token)
            .filter(|_| status == 0)
            .ok_or_else(|| protocol_error("Worker supervisor returned an invalid token receipt"))
    }
}

#[derive(Clone, Copy, Default)]
struct Stop {
    signal: u8,
    channel_ended: bool,
    acknowledged: bool,
}

pub(super) struct Lease {
    peer: Arc<Peer>,
    token: String,
    wire: String,
    stop: Mutex<Stop>,
    changed: Notify,
    proof: watch::Sender<Option<Result<Proof, Failure>>>,
}

impl Lease {
    fn new(peer: Arc<Peer>, command: &str) -> Self {
        let token = format!("{:032x}", rand::random::<u128>());
        let wire = format!(
            "/usr/bin/perl {}/supervisor.pl run {} {} {token} /bin/sh -c {}",
            peer.root,
            peer.root,
            peer.timing.lease_ms,
            quote(command)
        );
        Self {
            peer,
            token,
            wire,
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
        self.peer.connection.runtime.spawn(Box::pin(async move {
            let outcome = lease.watch().await.map_err(|error| error.failure().clone());
            if let Some(receipt) = receipt {
                receipt.end(match &outcome {
                    Ok(_) => TransferCleanup::Confirmed,
                    Err(why) => TransferCleanup::Unconfirmed(why.clone()),
                });
            }
            lease.proof.send_replace(Some(outcome));
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

    pub(super) fn acknowledge(&self) {
        self.stop.lock().unwrap_or_else(|error| error.into_inner()).acknowledged = true;
        self.changed.notify_one();
    }

    pub(super) fn channel_ended(&self) {
        self.stop
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .channel_ended = true;
        self.changed.notify_one();
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
        let mut signalled = 0;
        let mut delay = self.peer.timing.interval;
        loop {
            tokio::select! {
                _ = self.changed.notified() => {},
                _ = tokio::time::sleep(delay) => {},
            }
            let state = *self.stop.lock().unwrap_or_else(|error| error.into_inner());
            let now = tokio::time::Instant::now();
            if (state.signal != 0 || state.channel_ended) && ending.is_none() {
                ending = Some(now);
            }
            let wanted = if state.channel_ended || ending.is_some_and(|start| now - start >= CLOSE_GRACE) {
                3
            } else {
                state.signal
            };
            let operation = if wanted > signalled {
                wanted.to_string()
            } else if wanted == 0
                && state.acknowledged
                && let Some(stamp) = stamp
            {
                format!("renew-{stamp}")
            } else {
                "status".to_string()
            };
            match self.peer.ask(&self.token, &operation).await {
                Ok(Reply::Proof(proof)) => return Ok(proof),
                Ok(Reply::Unconfirmed) => {
                    return Err(protocol_error("Worker process group disappearance was not confirmed"));
                }
                Ok(Reply::Running(expiry)) => {
                    signalled = wanted;
                    let needs_renewal = stamp.is_none() && ending.is_none();
                    stamp = Some(expiry);
                    delay = if needs_renewal || ending.is_some() {
                        WallDuration::from_millis(100)
                    } else {
                        self.peer.timing.interval
                    };
                }
                Ok(Reply::Busy | Reply::Unknown) => delay = WallDuration::from_millis(100),
                Err(error) if !self.peer.connection.is_connected() => return Err(error),
                Err(_) => delay = WallDuration::from_secs(1),
            }
            if ending.is_some_and(|start| now - start >= CLOSE_GRACE + 3 * CONTROL_TIMEOUT) {
                return Err(protocol_error("Worker exit proof did not arrive during cleanup"));
            }
        }
    }
}
