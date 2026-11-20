//! D96 worker lease and process-group receipts.

mod control;

use super::{channel::Stream, client::SshConnection, config::SshLogging};
use crate::cmd::{TransferCleanup, TransferReceipt, failure};
use base64::{Engine, engine::general_purpose::STANDARD};
use control::ControlSession;
use jcompat::jtime::Duration;
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    executor::shared::SharedDeferred,
};
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
    sync::{Notify, watch},
    time::{Instant, sleep_until, timeout_at},
};

const TARGET: &str = "marathon_apple::cmd::remote::ssh::sshj::supervisor";
const PROTOCOL: &str = "MARATHON_D96_1";
const PROBE_TIMEOUT: WallDuration = WallDuration::from_secs(12);
const CONTROL_TIMEOUT: WallDuration = WallDuration::from_secs(4);
const REQUEST_TIMEOUT: WallDuration = WallDuration::from_secs(2 * CONTROL_TIMEOUT.as_secs());
pub(super) const EXIT_PROOF_TIMEOUT: WallDuration = WallDuration::from_secs(24);
const RECONNECT_DELAY: WallDuration = WallDuration::from_secs(1);
const CLOSE_GRACE: WallDuration = WallDuration::from_secs(20);
const WATCH_EXIT_TIMEOUT: WallDuration = WallDuration::from_secs(CLOSE_GRACE.as_secs() + 3 * CONTROL_TIMEOUT.as_secs());
// D98: how often, and for how long after launch, a detached recorder's start is polled on an open control stream.
const START_POLL: WallDuration = WallDuration::from_millis(200);
const START_POLL_WINDOW: WallDuration = WallDuration::from_secs(60);
const BOOTSTRAP: &str = include_str!("bootstrap.pl");
const WORKER: &str = include_str!("worker.pl");

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Waker};

    #[test]
    fn recorder_stderr_is_bounded_and_only_accepted_on_a_failed_receipt_d96() {
        let stderr = STANDARD.encode(b"A recording is already in progress\r\n");
        assert_eq!(
            Reply::parse(&format!("token done:16 {stderr}\n"), "token"),
            Some((
                Reply::Proof(Proof::Done(16)),
                Some("A recording is already in progress\r\n".into())
            ))
        );
        assert_eq!(Reply::parse(&format!("token done:0 {stderr}\n"), "token"), None);
        let too_large = STANDARD.encode(vec![b'x'; 8193]);
        assert_eq!(Reply::parse(&format!("token done:1 {too_large}\n"), "token"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn retirement_waits_for_the_in_flight_probe_d96() {
        let supervisor = Supervisor::new(
            ("worker".into(), 22),
            Arc::default(),
            SshLogging::new(Arc::default(), Arc::new(|_, _, _, _| {})),
            Duration::ZERO,
            1,
            Arc::new(|| panic!("retirement must not dial")),
        );
        let reply = SharedDeferred::new();
        *supervisor.capability.lock().unwrap() = Capability::Probing(reply.clone());
        let mut retiring = std::pin::pin!(supervisor.retire());
        let mut context = Context::from_waker(Waker::noop());
        assert!(retiring.as_mut().poll(&mut context).is_pending());
        reply.complete(Ok(None));
        assert!(retiring.as_mut().poll(&mut context).is_ready());
    }
}

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

#[derive(Clone, Default)]
enum Capability {
    #[default]
    Unknown,
    Probing(Arc<SharedDeferred<Option<Arc<Peer>>>>),
    Ready(Option<Arc<Peer>>),
}

pub(super) struct Supervisor {
    capability: Mutex<Capability>,
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
            capability: Mutex::default(),
            dial,
            worker,
            warnings,
            logging,
            timing: Timing::from_keep_alive(interval, count),
        }
    }

    pub(super) async fn available(self: &Arc<Self>, connection: &Arc<SshConnection>) -> VendorResult<bool> {
        Ok(self.peer(connection).await?.is_some())
    }

    pub(super) async fn prepare(
        self: &Arc<Self>,
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
        self: &Arc<Self>,
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
        let capability = self
            .capability
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        // The bootstrap can create its directory after its last caller stops waiting.
        let peer = match capability {
            Capability::Probing(reply) => match tokio::time::timeout(PROBE_TIMEOUT, reply.wait()).await {
                Ok(Ok(peer)) => (*peer).clone(),
                _ => None,
            },
            Capability::Ready(peer) => peer,
            Capability::Unknown => None,
        };
        let Some(peer) = peer else { return };
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let result = timeout_at(deadline, peer.retire(deadline)).await;
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

    async fn peer(self: &Arc<Self>, connection: &Arc<SshConnection>) -> VendorResult<Option<Arc<Peer>>> {
        let (reply, launch) = {
            let mut capability = self.capability.lock().unwrap_or_else(|error| error.into_inner());
            match &*capability {
                Capability::Ready(peer) => return Ok(peer.clone()),
                Capability::Probing(reply) => (reply.clone(), false),
                Capability::Unknown => {
                    let reply = SharedDeferred::new();
                    *capability = Capability::Probing(reply.clone());
                    (reply, true)
                }
            }
        };
        if launch {
            // The attempt belongs to the connection. A caller abandoning its
            // wait cannot restart the probe for the other waiting commands.
            let supervisor = self.clone();
            let connection = connection.clone();
            let runtime = connection.runtime.clone();
            let completed = reply.clone();
            runtime.spawn(Box::pin(async move {
                let result = crate::cmd::retained_tasks::panic_as_error(supervisor.install(connection)).await;
                let mut capability = supervisor.capability.lock().unwrap_or_else(|error| error.into_inner());
                *capability = match &result {
                    Ok(peer) => Capability::Ready(peer.clone()),
                    Err(_) => Capability::Unknown,
                };
                completed.complete(result);
                Ok(())
            }));
        }
        reply.wait().await.map(|peer| (*peer).clone())
    }

    async fn install(&self, connection: Arc<SshConnection>) -> VendorResult<Option<Arc<Peer>>> {
        let install = format!(
            "/usr/bin/perl -MMIME::Base64 -e 'eval decode_base64 shift; die $@ if $@' -- {} {}",
            STANDARD.encode(BOOTSTRAP),
            STANDARD.encode(WORKER)
        );
        let (status, output) = exchange(&connection, install, Instant::now() + PROBE_TIMEOUT)
            .await
            .map_err(ExchangeError::into_vendor)?;
        if status != 0 {
            let first = self
                .warnings
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(self.worker.clone());
            if first {
                let logger = self.logging.component(TARGET);
                self.logging.log(&logger, Level::WARN, || {
                    format!("SSH worker {}:{} has no usable /usr/bin/perl with the required core modules. Leftover worker processes are not contained.", self.worker.0, self.worker.1)
                }, None);
            }
            return Ok(None);
        }
        let root = output
            .trim()
            .strip_prefix(&format!("{PROTOCOL} "))
            .filter(|root| valid_root(root))
            .ok_or_else(|| protocol_error("SSH worker returned an invalid supervisor bootstrap response"))?;
        Ok(Some(Arc::new(Peer {
            data: connection,
            dial: self.dial.clone(),
            control: tokio::sync::Mutex::default(),
            root: root.to_owned(),
            timing: self.timing,
            logging: self.logging.clone(),
        })))
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

// An escalation may interrupt an earlier control exchange. Return its channel slot.
struct ProtocolChannel(Arc<super::channel::ExecChannel>);

impl Drop for ProtocolChannel {
    fn drop(&mut self) {
        self.0.request_close();
    }
}

async fn exchange(connection: &SshConnection, wire: String, deadline: Instant) -> Result<(i32, String), ExchangeError> {
    let channel = timeout_at(deadline, connection.start_session())
        .await
        .map_err(|_| ExchangeError::Timeout("Worker supervisor channel open timed out"))?
        .map_err(|error| ExchangeError::Command(error.into_vendor()))?;
    let closing = ProtocolChannel(channel);
    let channel = &closing.0;
    timeout_at(deadline, async {
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
    .await
    .map_err(|_| ExchangeError::Timeout("Worker supervisor command timed out"))?
    .map_err(ExchangeError::Command)
}

#[derive(Default)]
struct Control {
    active: Option<Arc<SshConnection>>,
    stream: Option<Arc<SharedDeferred<Arc<ControlSession>>>>,
    next_dial: Option<Instant>,
    retry_delay: Option<WallDuration>,
}
struct Peer {
    data: Arc<SshConnection>,
    dial: ControlDial,
    control: tokio::sync::Mutex<Control>,
    root: String,
    timing: Timing,
    logging: SshLogging,
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
    /// The expiry, and whether a detached recorder has printed its start marker (D98).
    Running(u64, bool),
    Busy,
    Unknown,
    Proof(Proof),
    Unconfirmed,
}

impl Reply {
    fn parse(output: &str, token: &str) -> Option<(Self, Option<String>)> {
        let (returned, state) = output.strip_suffix('\n')?.split_once(' ')?;
        if returned != token {
            return None;
        }
        let (state, encoded) = state
            .split_once(' ')
            .map_or((state, None), |(state, encoded)| (state, Some(encoded)));
        let reply = match state {
            "busy" => Self::Busy,
            "unknown" => Self::Unknown,
            "not-started" => Self::Proof(Proof::NotStarted),
            "unconfirmed" => Self::Unconfirmed,
            _ if state.starts_with("running:") => Self::Running(state[8..].parse().ok()?, false),
            _ if state.starts_with("recording:") => Self::Running(state[10..].parse().ok()?, true),
            _ if state.starts_with("done:") => Self::Proof(Proof::Done(i32::from(state[5..].parse::<u8>().ok()?))),
            _ => return None,
        };
        let diagnostic = match encoded {
            Some(encoded) if matches!(reply, Self::Proof(Proof::Done(1..)) | Self::Unconfirmed) => {
                if encoded.len() > 8192usize.div_ceil(3) * 4 {
                    return None;
                }
                let bytes = STANDARD.decode(encoded).ok()?;
                if bytes.len() > 8192 {
                    return None;
                }
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
            Some(_) => return None,
            None => None,
        };
        Some((reply, diagnostic))
    }
}

impl Peer {
    async fn retire(&self, deadline: Instant) -> VendorResult<()> {
        let connection = if self.data.is_connected() {
            self.data.clone()
        } else {
            let (active, stream) = {
                let mut control = self.control.lock().await;
                let active = control
                    .active
                    .clone()
                    .filter(|active| active.is_connected())
                    .ok_or_else(|| protocol_error("Worker supervisor has no remaining connection"))?;
                (active, control.stream.take())
            };
            // A worker can allow only one session on this connection.
            if let Some(stream) = stream
                && let Ok(open) = stream.wait().await
            {
                open.close().await?;
            }
            active
        };
        let wire = format!(
            "/usr/bin/perl {}/supervisor.pl retire {} {}",
            self.root, self.root, self.timing.lease_ms
        );
        loop {
            let (status, output) = exchange(&connection, wire.clone(), deadline)
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
        let deadline = Instant::now() + CONTROL_TIMEOUT;
        let mut control = timeout_at(deadline, self.control.lock())
            .await
            .map_err(|_| protocol_error("Worker supervisor reconnect is busy"))?;
        if let Some(active) = &control.active
            && active.is_connected()
        {
            return Ok(active.clone());
        }
        if control.next_dial.is_some_and(|next| Instant::now() < next) {
            return Err(protocol_error("Worker supervisor reconnect is waiting to retry"));
        }
        // One dial per shared connection. Its deadline also bounds callers
        // waiting behind it. Keep the failed-attempt bound on cancellation.
        let retry = control.retry_delay.unwrap_or(RECONNECT_DELAY);
        control.next_dial = Some(deadline + retry);
        control.retry_delay = Some((retry * 2).min(WallDuration::from_secs(60)));
        let connected = timeout_at(deadline, (self.dial)()).await;
        control.next_dial = Some(Instant::now() + retry);
        let active = connected
            .map_err(|_| protocol_error("Worker supervisor control connection timed out"))?
            .map_err(super::error::SshFailure::into_vendor)?;
        control.active = Some(active.clone());
        control.stream = None;
        Ok(active)
    }

    /// An established control stream, which answers a status request without dialling or using a data session.
    fn stream_ready(&self) -> bool {
        self.control.try_lock().is_ok_and(|control| {
            control.active.as_ref().is_some_and(|active| active.is_connected())
                && control
                    .stream
                    .as_ref()
                    .and_then(|stream| stream.completed())
                    .is_some_and(|stream| stream.as_ref().is_ok_and(|stream| stream.is_open()))
        })
    }

    async fn control_lost(&self) {
        let control = self.control.try_lock().ok().and_then(|control| {
            let active = control.active.as_ref()?.clone();
            let stream = control.stream.as_ref().and_then(|stream| stream.completed()?.ok());
            Some((active, stream))
        });
        match control {
            Some((active, Some(stream))) if active.is_connected() && stream.is_open() => {
                tokio::select! {
                    _ = active.shared.wait_dead() => {}
                    _ = stream.wait_closed() => {}
                }
            }
            Some((active, _)) if active.is_connected() => active.shared.wait_dead().await,
            _ => std::future::pending().await,
        }
    }

    async fn stream(&self) -> VendorResult<(Arc<SshConnection>, Arc<ControlSession>)> {
        let active = self.connection().await?;
        let opening = {
            let mut control = self.control.lock().await;
            if !control
                .active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &active))
            {
                return Err(protocol_error("Worker supervisor control reconnected"));
            }
            if let Some(opening) = &control.stream
                && match opening.completed() {
                    None => true,
                    Some(Ok(stream)) => stream.is_open(),
                    Some(Err(_)) => false,
                }
            {
                opening.clone()
            } else {
                let opening = SharedDeferred::new();
                control.stream = Some(opening.clone());
                let wire = format!(
                    "/usr/bin/perl {}/supervisor.pl control-stream {} {}",
                    self.root, self.root, self.timing.lease_ms
                );
                let opened = opening.clone();
                let connection = active.clone();
                active.runtime.spawn(Box::pin(async move {
                    let result = tokio::time::timeout(
                        REQUEST_TIMEOUT,
                        crate::cmd::retained_tasks::panic_as_error(ControlSession::open(&connection, wire)),
                    )
                    .await;
                    opened.complete(match result {
                        Ok(result) => result,
                        Err(_) => {
                            // A silent open cannot start sending token requests.
                            connection.abandon();
                            Err(protocol_error("Worker supervisor control stream did not open"))
                        }
                    });
                    Ok(())
                }));
                opening
            }
        };
        let stream = opening.wait().await?;
        if stream.is_open()
            && let Ok(mut control) = self.control.try_lock()
            && control
                .active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &active))
        {
            control.retry_delay = None;
            control.next_dial = None;
        }
        Ok((active, (*stream).clone()))
    }

    async fn control_request(
        &self,
        token: &str,
        operation: &str,
        deadline: Instant,
        may_hold_reply: bool,
    ) -> VendorResult<String> {
        let first_timeout = Instant::now() + CONTROL_TIMEOUT;
        let fallback = || async {
            let wire = format!(
                "/usr/bin/perl {}/supervisor.pl control {} {} {token}:{operation}",
                self.root, self.root, self.timing.lease_ms
            );
            let deadline = deadline.min(Instant::now() + CONTROL_TIMEOUT);
            let (status, output) = exchange(&self.data, wire, deadline)
                .await
                .map_err(ExchangeError::into_vendor)?;
            if status != 0 {
                return Err(protocol_error("Worker supervisor control request failed"));
            }
            Ok(output)
        };
        let opening = async {
            let (connection, stream) = self.stream().await?;
            let request = stream.request(token, operation)?;
            Ok((connection, request))
        };
        let stalled = || protocol_error("Worker supervisor did not respond on its control stream");
        let (connection, mut request) = match timeout_at(first_timeout.min(deadline), opening).await {
            Ok(Ok(request)) => request,
            _ if self.data.is_connected() && Instant::now() < deadline => return fallback().await,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(stalled()),
        };
        match timeout_at(first_timeout.min(deadline), request.reply()).await {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(_)) if self.data.is_connected() && Instant::now() < deadline => return fallback().await,
            Ok(Err(error)) => return Err(error),
            Err(_) if Instant::now() >= deadline => return Err(stalled()),
            Err(_) if !may_hold_reply => request.timed_out(&connection),
            // A detached recorder holds INT until it can report an exit receipt.
            Err(_) => {}
        }
        if !self.data.is_connected() {
            if !connection.is_connected() {
                return Err(stalled());
            }
            return timeout_at(deadline, request.reply()).await.map_err(|_| stalled())?;
        }
        // A slow reply can still win after the data-path request has started.
        let mut secondary = std::pin::pin!(fallback());
        tokio::select! {
            biased;
            response = request.reply() => match response {
                Ok(output) => Ok(output),
                Err(_) => secondary.await,
            },
            response = &mut secondary => response,
        }
    }

    async fn ask(&self, token: &str, operation: &str, deadline: Instant, may_hold_reply: bool) -> VendorResult<Reply> {
        let output = self.control_request(token, operation, deadline, may_hold_reply).await?;
        let (reply, diagnostic) = Reply::parse(&output, token)
            .ok_or_else(|| protocol_error("Worker supervisor returned an invalid token receipt"))?;
        if let Some(stderr) = diagnostic {
            let logger = self.logging.component(TARGET);
            self.logging.log(
                &logger,
                Level::WARN,
                || {
                    let mut message = format!("Worker recorder {token} failed ({reply:?}). stderr: {stderr:?}");
                    if stderr.contains("A recording is already in progress") {
                        message.push_str(
                            ". Stop the active recording or restart the affected simulator before recording again.",
                        );
                    }
                    message
                },
                None,
            );
        }
        Ok(reply)
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
        // A successful rsync waited for its remote shell. The supervised
        // sender returns zero only after recording its cleanup receipt.
        self.lease.complete_with_status(0);
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
    started: watch::Sender<bool>,
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
            started: watch::channel(false).0,
        }
    }
    pub(super) fn wire(&self) -> &str {
        &self.wire
    }

    pub(super) fn start(self: &Arc<Self>, receipt: Option<Arc<TransferReceipt>>) {
        if self.detached {
            // Set up the independent stream while the recorder is running,
            // before its first stop needs a receipt.
            let peer = self.peer.clone();
            self.peer.data.runtime.spawn(Box::pin(async move {
                let _ = peer.stream().await;
                Ok(())
            }));
        }
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
        if rank > state.signal {
            state.signal = rank;
            self.changed.notify_one();
        }
    }

    pub(super) fn is_detached(&self) -> bool {
        self.detached
    }

    /// D98: true once the worker has seen a detached recorder's own start marker. The worker holds INT until then.
    pub(super) fn started(&self) -> watch::Receiver<bool> {
        self.started.subscribe()
    }

    pub(super) fn acknowledge(&self) {
        if !self.detached {
            self.stop.lock().unwrap_or_else(|error| error.into_inner()).acknowledged = true;
        }
    }

    pub(super) fn data_status(&self, status: i32) {
        if self.detached && status == 0 {
            self.stop.lock().unwrap_or_else(|error| error.into_inner()).acknowledged = true;
            // D98: the watcher starts looking for the recorder's start marker at launch, not one interval later.
            self.changed.notify_one();
        }
        if !self.foreground {
            return;
        }
        // The wrapper reports a normal status only after it proves cleanup.
        // A channel close, or the wrapper's failure signal, is never evidence.
        self.complete_with_status(status);
    }

    fn complete_with_status(&self, status: i32) {
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

    async fn stop_upgraded(&self, wanted: u8) {
        loop {
            let changed = self.changed.notified();
            let state = *self.stop.lock().unwrap_or_else(|error| error.into_inner());
            if state.signal > wanted || state.channel_ended {
                return;
            }
            changed.await;
        }
    }

    async fn watch(&self) -> VendorResult<Proof> {
        let mut stamp = None;
        let mut ending = None;
        let mut next_renewal = Instant::now();
        // D98: the acknowledgement of a detached recorder wakes the watcher early. Renewal keeps this schedule.
        let first_renewal = next_renewal + self.peer.timing.interval;
        let mut signalled = 0;
        let mut delay = self.peer.timing.interval;
        let mut start_poll_until = None;
        let mut start_poll = false;
        loop {
            let mut polled = false;
            if !delay.is_zero() {
                let wake = Instant::now() + delay;
                tokio::select! {
                    _ = self.changed.notified() => {},
                    _ = self.peer.control_lost() => {},
                    _ = self.peer.data.shared.wait_dead(), if self.detached && self.peer.data.is_connected() => {},
                    _ = sleep_until(ending.map_or(wake, |start| wake.min(start + WATCH_EXIT_TIMEOUT))) => polled = start_poll,
                }
            }
            start_poll = false;
            let state = *self.stop.lock().unwrap_or_else(|error| error.into_inner());
            let now = Instant::now();
            let disconnected = state.channel_ended || (self.detached && !self.peer.data.is_connected());
            let finalizing = self.detached && state.signal == 1 && !disconnected;
            if !finalizing && (state.signal != 0 || disconnected) && ending.is_none() {
                ending = Some(now);
            }
            let proof_deadline = ending.map(|start| start + WATCH_EXIT_TIMEOUT);
            if proof_deadline.is_some_and(|deadline| now >= deadline) {
                return Err(protocol_error("Worker exit proof did not arrive during cleanup"));
            }
            let grace_end = ending.map(|start| start + CLOSE_GRACE);
            let wanted = if disconnected || grace_end.is_some_and(|deadline| now >= deadline) {
                3
            } else {
                state.signal
            };
            let operation = if wanted > signalled {
                wanted.to_string()
            } else if polled && now < next_renewal.max(first_renewal) {
                "status".to_string()
            } else if (wanted == 0 || (finalizing && now >= next_renewal))
                && state.acknowledged
                && let Some(stamp) = stamp
            {
                format!("renew-{stamp}")
            } else {
                "status".to_string()
            };
            let request_end = now + REQUEST_TIMEOUT;
            let request_end = proof_deadline.map_or(request_end, |deadline| request_end.min(deadline));
            let may_hold_reply = self.detached && operation == "1";
            let response = tokio::select! {
                biased;
                _ = self.stop_upgraded(wanted), if wanted < 3 => {
                    delay = WallDuration::ZERO;
                    continue;
                }
                _ = self.peer.data.shared.wait_dead(), if self.detached && wanted < 3 => {
                    delay = WallDuration::ZERO;
                    continue;
                }
                _ = sleep_until(grace_end.unwrap_or(request_end)), if wanted < 3 && grace_end.is_some() => {
                    delay = WallDuration::ZERO;
                    continue;
                }
                _ = sleep_until(proof_deadline.unwrap_or(request_end)), if proof_deadline.is_some() => {
                    return Err(protocol_error("Worker exit proof did not arrive during cleanup"));
                }
                response = self.peer.ask(&self.token, &operation, request_end, may_hold_reply) => response,
            };
            match response {
                Ok(Reply::Proof(proof)) => return Ok(proof),
                Ok(Reply::Unconfirmed) => {
                    return Err(protocol_error("Worker process group disappearance was not confirmed"));
                }
                Ok(Reply::Running(expiry, started)) => {
                    if started {
                        self.started.send_replace(true);
                    }
                    signalled = wanted;
                    if operation.starts_with("renew-") {
                        next_renewal = now + self.peer.timing.interval;
                    }
                    let early = now < first_renewal;
                    let needs_renewal = stamp.is_none() && ending.is_none() && !early;
                    stamp = Some(expiry);
                    // D98: until the recorder confirms its start, poll a little after launch so its start is logged
                    // when it happens. Only an open stream is used; renewals keep their own schedule.
                    start_poll = self.detached
                        && state.acknowledged
                        && wanted == 0
                        && ending.is_none()
                        && !*self.started.borrow()
                        && now < *start_poll_until.get_or_insert(now + START_POLL_WINDOW)
                        && self.peer.stream_ready();
                    delay = if needs_renewal {
                        WallDuration::ZERO
                    } else if ending.is_some() || finalizing {
                        WallDuration::from_millis(100)
                    } else if start_poll {
                        START_POLL.min(self.peer.timing.interval)
                    } else if early && signalled == 0 {
                        first_renewal - now
                    } else {
                        self.peer.timing.interval
                    };
                }
                Ok(Reply::Busy | Reply::Unknown) => delay = WallDuration::from_millis(100),
                Err(_) => delay = WallDuration::from_secs(1),
            }
        }
    }
}
