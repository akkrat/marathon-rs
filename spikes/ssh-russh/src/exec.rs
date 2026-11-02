//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/
//! (SshjCommandExecutorFactory, SshjCommandExecutor and SshjCommandSession).
//! Spike code: it measures what russh can do, it is not the WP 4.2 design.

// The session keeps the whole SshjCommandSession surface (kill, the timeout flag) even where no case uses it
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use russh::client::{self, DisconnectReason};
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, ChannelWriteHalf, Sig};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::lines::LineBuffer;
use crate::quote::shell_command;

#[derive(Clone, Debug)]
pub enum Auth {
    Password(String),
    Key(PathBuf),
    /// Spike-only: the remote-mac worker accepts only a key held by the local ssh-agent
    Agent,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub known_hosts: Option<PathBuf>,
    pub keep_alive: Duration,
    pub keep_alive_max: usize,
    pub attempts: u32,
}

/// sshj takes the keep-alive interval in whole seconds (`keepAliveInterval.toSeconds().toInt()`): 1500 ms becomes 1 s and
/// anything below a second disables keep-alive, which russh expresses as `None`.
pub fn keep_alive_interval(configured: Duration) -> Option<Duration> {
    match configured.as_secs() {
        0 => None,
        seconds => Some(Duration::from_secs(seconds)),
    }
}

/// The client was rejected by the server during authentication
#[derive(Debug)]
pub struct AuthFailed(pub String);

impl std::fmt::Display for AuthFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "authentication failed: {}", self.0)
    }
}

impl std::error::Error for AuthFailed {}

/// The server sent SSH_MSG_DISCONNECT while connecting. russh reports only `Error::Disconnect` from `connect()`; the
/// reason code, which the transient classification needs (sshj's `TransportException.disconnectReason`), arrives in
/// `Handler::disconnected` and is carried here
#[derive(Debug)]
pub struct PeerDisconnected(pub u32, pub String);

impl std::fmt::Display for PeerDisconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "received disconnect, reason {}: {}", self.0, self.1)
    }
}

impl std::error::Error for PeerDisconnected {}

pub struct Handler {
    host: String,
    port: u16,
    known_hosts: Option<PathBuf>,
    /// Why the connection ended, for commands in flight
    lost: Arc<Mutex<Option<String>>>,
    /// The reason of a DISCONNECT received from the server
    peer_disconnect: Arc<Mutex<Option<(u32, String)>>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let Some(path) = &self.known_hosts else {
            return Ok(true);
        };
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        Ok(keys::check_known_hosts_path(&self.host, self.port, key, path)?)
    }

    async fn disconnected(&mut self, reason: DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        let (text, result) = match reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                let text = format!("received disconnect {:?}: {}", info.reason_code, info.message);
                if let Ok(mut peer) = self.peer_disconnect.lock() {
                    *peer = Some((info.reason_code as u32, info.message));
                }
                (text, Ok(()))
            }
            DisconnectReason::Error(e) => (format!("{e} ({e:?})"), Err(e)),
        };
        if let Ok(mut lost) = self.lost.lock() {
            *lost = Some(text);
        }
        result
    }
}

pub struct Executor {
    handle: client::Handle<Handler>,
    pub host_id: String,
    lost: Arc<Mutex<Option<String>>>,
}

/// Connects with up to `attempts` fresh clients and no delay, then authenticates once, as SshjCommandExecutorFactory does.
/// Host-key verification is part of the key exchange and is retried with it, as in sshj.
pub async fn connect(target: &Target, attempts: &AtomicU32) -> anyhow::Result<Executor> {
    let config = Arc::new(client::Config {
        keepalive_interval: keep_alive_interval(target.keep_alive),
        keepalive_max: target.keep_alive_max,
        ..Default::default()
    });
    let lost = Arc::new(Mutex::new(None));
    let peer_disconnect = Arc::new(Mutex::new(None));
    let mut attempt = 1;
    let mut handle = loop {
        attempts.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut peer) = peer_disconnect.lock() {
            *peer = None;
        }
        if let Ok(mut reason) = lost.lock() {
            *reason = None;
        }
        let handler = Handler {
            host: target.host.clone(),
            port: target.port,
            known_hosts: target.known_hosts.clone(),
            lost: lost.clone(),
            peer_disconnect: peer_disconnect.clone(),
        };
        match client::connect(config.clone(), (target.host.as_str(), target.port), handler).await {
            Ok(handle) => break handle,
            Err(e) if attempt < target.attempts => {
                log::warn!("Retrying after failure: {e}");
                attempt += 1;
            }
            Err(e) => {
                let peer = peer_disconnect.lock().ok().and_then(|p| p.clone());
                return match peer {
                    Some((reason, message)) => Err(anyhow::Error::new(PeerDisconnected(reason, message)).context(e)),
                    None => Err(e).context("connect"),
                };
            }
        }
    };
    if let Err(e) = authenticate(&mut handle, target).await {
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "").await;
        return Err(e);
    }
    Ok(Executor {
        handle,
        host_id: format!("{}@{}:{}", target.user, target.host, target.port),
        lost,
    })
}

async fn authenticate(handle: &mut client::Handle<Handler>, target: &Target) -> anyhow::Result<()> {
    let result = match &target.auth {
        Auth::Password(password) => handle.authenticate_password(&target.user, password).await?,
        Auth::Key(path) => {
            let key = keys::load_secret_key(path, None).context("load key")?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(&target.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        }
        Auth::Agent => {
            let mut agent = keys::agent::client::AgentClient::connect_env().await?;
            let identity = agent
                .request_identities()
                .await?
                .into_iter()
                .find(|id| id.public_key().algorithm() == keys::Algorithm::Ed25519)
                .ok_or_else(|| anyhow!("the agent holds no ssh-ed25519 key"))?;
            let key = identity.public_key().into_owned();
            handle
                .authenticate_publickey_with(&target.user, key, None, &mut agent)
                .await
                .map_err(|e| anyhow!("agent: {e:?}"))?
        }
    };
    if !result.success() {
        bail!(AuthFailed(format!("{result:?}")));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum ExecError {
    /// The connection ended while the command ran: keep-alive loss, a dropped transport
    ConnectionLost(String),
    /// No output on either stream for longer than the idle timeout
    IdleTimeout(String),
    /// Extended data of a type other than stderr (sshj: ConnectionException PROTOCOL_ERROR)
    Protocol(String),
    /// `AbstractChannel.close()` gave up waiting for the peer's CLOSE (sshj: ConnectionException <- TimeoutException)
    CloseTimeout(String),
    /// The server answered the exec request with failure
    ExecRejected(String),
    /// The connection is gone before the command could start (sshj: `IllegalStateException("Not connected")` from
    /// `PatchedSSHClient.startSession`, which is not an IOException)
    NotConnected,
    Cancelled,
    Other(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ExecError {}

#[derive(Clone, Debug, Default)]
pub struct ExitInfo {
    pub status: Option<u32>,
    pub signal: Option<String>,
}

impl ExitInfo {
    /// `exitStatus ?: autodetectExitCode(exitSignal)`: TERM is 143, any other signal is null
    pub fn exit_code(&self) -> Option<i32> {
        match (&self.status, self.signal.as_deref()) {
            (Some(status), _) => Some(*status as i32),
            (None, Some("TERM")) => Some(143),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct CommandResult {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub exit_code: Option<i32>,
    pub raw: ExitInfo,
}

impl CommandResult {
    pub fn show(&self) -> String {
        let exit = self.exit_code.map_or("null".to_string(), |c| c.to_string());
        format!(
            "exit={exit} stdout={} stderr={}",
            kotlin_list(&self.stdout),
            kotlin_list(&self.stderr)
        )
    }
}

/// Kotlin's List.toString()
pub fn kotlin_list(items: &[String]) -> String {
    format!("[{}]", items.join(", "))
}

type Lines = mpsc::Receiver<Result<String, ExecError>>;

/// Channel.BUFFERED in kotlinx.coroutines
const CHANNEL_CAPACITY: usize = 64;
/// PollBackoff.maxMillis: the Kotlin readers poll, so its idle check allows one poll delay of slack
const POLL_SLACK: Duration = Duration::from_millis(20);
/// sshj's connection timeout (`Connection.getTimeoutMs`, 30 s by default), which bounds `AbstractChannel.close()`
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The channel as sshj's `Session.Command` sees it: `isOpen` turns false once a CLOSE was sent or received
struct ChannelState {
    write: ChannelWriteHalf<client::Msg>,
    close_sent: AtomicBool,
    /// Tells the readers to stop, as sshj's `canRead = isOpen && !isEOF` does after a local close
    closing: watch::Sender<bool>,
    done: watch::Receiver<bool>,
    /// Set when the channel ended with the peer's CLOSE, not with the connection
    closed_by_peer: Arc<AtomicBool>,
    lost: Arc<Mutex<Option<String>>>,
}

impl ChannelState {
    fn is_open(&self) -> bool {
        !self.close_sent.load(Ordering::SeqCst) && !*self.done.borrow()
    }

    /// `AbstractChannel.close()`: when open, send CLOSE and wait for the peer's CLOSE up to the connection timeout,
    /// failing with sshj's message after that; when not open, return at once
    async fn close(&self) -> Result<(), ExecError> {
        if !self.is_open() {
            return Ok(());
        }
        self.close_sent.store(true, Ordering::SeqCst);
        let _ = self.closing.send(true);
        let _ = self.write.close().await;
        let mut done = self.done.clone();
        match tokio::time::timeout(CONNECTION_TIMEOUT, async { done.wait_for(|d| *d).await.map(|_| ()) }).await {
            Ok(_) => self.outcome(),
            Err(_) => Err(ExecError::CloseTimeout(format!(
                "Timeout expired: {} MILLISECONDS",
                CONNECTION_TIMEOUT.as_millis()
            ))),
        }
    }

    /// `Channel.join()` without a timeout; like sshj's close event it fails when the connection went away instead
    async fn join(&self) -> Result<(), ExecError> {
        let mut done = self.done.clone();
        let _ = done.wait_for(|d| *d).await;
        self.outcome()
    }

    fn outcome(&self) -> Result<(), ExecError> {
        if self.closed_by_peer.load(Ordering::SeqCst) {
            return Ok(());
        }
        let reason = self.lost.lock().ok().and_then(|l| l.clone()).unwrap_or_default();
        Err(ExecError::ConnectionLost(reason))
    }
}

pub struct CommandSession {
    pub stdout: Option<Lines>,
    pub stderr: Option<Lines>,
    exit: Option<JoinHandle<Result<ExitInfo, ExecError>>>,
    exit_abort: tokio::task::AbortHandle,
    state: Arc<ChannelState>,
    /// SshjCommandSession's `isAlive = command.isOpen` is evaluated once, at construction (known bug A11)
    alive_at_construction: bool,
    closed: AtomicBool,
}

impl Executor {
    pub fn connected(&self) -> bool {
        !self.handle.is_closed()
    }

    /// One session channel and exec request per command; `env` and `workdir` are not sent, as in the Kotlin executor
    pub async fn execute(
        &self,
        command: &[String],
        timeout: Duration,
        idle_timeout: Option<Duration>,
    ) -> anyhow::Result<CommandSession> {
        if self.handle.is_closed() {
            bail!(ExecError::NotConnected);
        }
        let channel = match self.handle.channel_open_session().await {
            Ok(channel) => channel,
            Err(russh::Error::SendError) => bail!(ExecError::NotConnected),
            Err(e) => return Err(e).context("open session channel"),
        };
        let (read, write) = channel.split();
        write.exec(true, shell_command(command)).await.context("exec")?;
        let (out_tx, out_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (err_tx, err_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (done_tx, done_rx) = watch::channel(false);
        let (closing_tx, closing_rx) = watch::channel(false);
        let state = Arc::new(ChannelState {
            write,
            close_sent: AtomicBool::new(false),
            closing: closing_tx,
            done: done_rx,
            closed_by_peer: Arc::new(AtomicBool::new(false)),
            lost: self.lost.clone(),
        });
        let (reply_tx, reply_rx) = oneshot::channel();
        let mut pump = tokio::spawn(pump(
            read,
            out_tx,
            err_tx,
            idle_timeout,
            done_tx,
            closing_rx,
            Some(reply_tx),
            state.closed_by_peer.clone(),
            self.lost.clone(),
        ));
        // sshj sends exec with want-reply and waits for the answer (up to the connection timeout); a rejection fails
        // execute() with a ConnectionException
        match tokio::time::timeout(CONNECTION_TIMEOUT, reply_rx).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => bail!(ExecError::ExecRejected("exec request failed".into())),
            // The channel ended before any reply: the connection went away
            Ok(Err(_)) => {
                let reason = self.lost.lock().ok().and_then(|l| l.clone()).unwrap_or_default();
                bail!(ExecError::ConnectionLost(reason))
            }
            Err(_) => bail!(ExecError::CloseTimeout("exec reply: Timeout expired".into())),
        }
        let exit_state = state.clone();
        let exit = tokio::spawn(async move {
            match tokio::time::timeout(timeout, &mut pump).await {
                Ok(joined) => joined.unwrap_or_else(|e| Err(ExecError::Other(e.to_string()))),
                Err(_) => {
                    // withTimeoutOrNull expired: `cmd.signal(TERM); cmd.close()`. The close waits for the peer's CLOSE
                    // (up to the connection timeout) and its failure fails the exit code; otherwise the code is null
                    let _ = exit_state.write.signal(Sig::TERM).await;
                    exit_state.close().await?;
                    Ok(ExitInfo::default())
                }
            }
        });
        Ok(CommandSession {
            stdout: Some(out_rx),
            stderr: Some(err_rx),
            exit_abort: exit.abort_handle(),
            exit: Some(exit),
            alive_at_construction: state.is_open(),
            state,
            closed: AtomicBool::new(false),
        })
    }

    /// CommandExecutor.safeExecute: a failure of `execute()` or of `await()` becomes None (the Kotlin code catches
    /// ConnectionException and TransportException around both)
    pub async fn safe_execute(&self, command: &[String], timeout: Duration) -> Option<CommandResult> {
        let mut session = self.execute(command, timeout, None).await.ok()?;
        session.await_result().await.ok()
    }

    pub async fn run(
        &self,
        command: &[&str],
        timeout: Duration,
        idle: Option<Duration>,
    ) -> anyhow::Result<CommandResult> {
        let command: Vec<String> = command.iter().map(|s| s.to_string()).collect();
        let mut session = self.execute(&command, timeout, idle).await?;
        Ok(session.await_result().await?)
    }

    pub async fn close(self) {
        let _ = self.handle.disconnect(russh::Disconnect::ByApplication, "", "").await;
    }
}

type Sender = mpsc::Sender<Result<String, ExecError>>;

/// Sends lines until the receiver is gone or the channel is closed locally, never blocking past either
async fn send_all(tx: &mut Option<Sender>, lines: Vec<String>, closing: &mut watch::Receiver<bool>) {
    for line in lines {
        let Some(sender) = tx.as_ref() else { return };
        if *closing.borrow() {
            return;
        }
        tokio::select! {
            sent = sender.send(Ok(line)) => if sent.is_err() { *tx = None; return },
            _ = async { closing.wait_for(|c| *c).await.map(|_| ()) } => return,
        }
    }
}

// A stopped Kotlin reader still sends its buffered tail. Keep this send independent of the
// channel pump so backpressure cannot prevent processing the peer's CLOSE.
fn drain_reader(tx: &mut Option<Sender>, buffer: &mut LineBuffer) {
    let lines = buffer.drain();
    if let Some(sender) = tx.take() {
        tokio::spawn(async move {
            for line in lines {
                if sender.send(Ok(line)).await.is_err() {
                    break;
                }
            }
        });
    }
}

async fn pump(
    mut read: russh::ChannelReadHalf,
    out_tx: Sender,
    err_tx: Sender,
    idle_timeout: Option<Duration>,
    done: watch::Sender<bool>,
    mut closing: watch::Receiver<bool>,
    mut exec_reply: Option<oneshot::Sender<bool>>,
    closed_by_peer: Arc<AtomicBool>,
    lost: Arc<Mutex<Option<String>>>,
) -> Result<ExitInfo, ExecError> {
    let (mut out_tx, mut err_tx) = (Some(out_tx), Some(err_tx));
    let (mut out_buf, mut err_buf) = (LineBuffer::default(), LineBuffer::default());
    let mut last_output = Instant::now();
    let mut exit = ExitInfo::default();
    let mut closed = false;
    let mut failure = None;
    let mut tick = tokio::time::interval(POLL_SLACK);
    let mut closing_seen = false;
    // The select below holds `closing` for its arm; the send path watches the same flag through a clone
    let mut send_closing = closing.clone();
    loop {
        tokio::select! {
            message = read.wait() => match message {
                Some(ChannelMsg::Data { data }) if !closing_seen => {
                    last_output = Instant::now();
                    out_buf.append(&data);
                    send_all(&mut out_tx, out_buf.flush(), &mut send_closing).await;
                }
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) if !closing_seen => {
                    last_output = Instant::now();
                    err_buf.append(&data);
                    send_all(&mut err_tx, err_buf.flush(), &mut send_closing).await;
                }
                Some(ChannelMsg::ExtendedData { ext, .. }) if !closing_seen => {
                    failure = Some(ExecError::Protocol(format!("Bad extended data type = {ext}")));
                    break;
                }
                Some(ChannelMsg::Eof) => {
                    // The Kotlin readers stop at EOF and drain their partial line
                    drain_reader(&mut out_tx, &mut out_buf);
                    drain_reader(&mut err_tx, &mut err_buf);
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => exit.status = Some(exit_status),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => exit.signal = Some(signal_name_of(&signal_name)),
                Some(ChannelMsg::Close) => {
                    closed = true;
                    closed_by_peer.store(true, Ordering::SeqCst);
                    break;
                }
                Some(ChannelMsg::Success) => { if let Some(r) = exec_reply.take() { let _ = r.send(true); } }
                Some(ChannelMsg::Failure) => { if let Some(r) = exec_reply.take() { let _ = r.send(false); } }
                Some(_) => {}
                None => break,
            },
            _ = async { closing.wait_for(|c| *c).await.map(|_| ()) }, if !closing_seen => {
                // A local close stops the readers; they drain their partial line, later data is dropped
                closing_seen = true;
                drain_reader(&mut out_tx, &mut out_buf);
                drain_reader(&mut err_tx, &mut err_buf);
            }
            _ = tick.tick(), if idle_timeout.is_some() && (out_tx.is_some() || err_tx.is_some()) => {
                let idle = idle_timeout.unwrap_or(Duration::MAX);
                if last_output.elapsed().saturating_sub(POLL_SLACK) > idle {
                    let error = ExecError::IdleTimeout(format!("idle timeout {idle:?} reached"));
                    for tx in [out_tx.take(), err_tx.take()].into_iter().flatten() {
                        let _ = tx.send(Err(error.clone())).await;
                    }
                }
            }
        }
    }
    if failure.is_none() {
        drain_reader(&mut out_tx, &mut out_buf);
        drain_reader(&mut err_tx, &mut err_buf);
    }
    if let Some(error) = &failure {
        for tx in [out_tx.take(), err_tx.take()].into_iter().flatten() {
            let _ = tx.send(Err(error.clone())).await;
        }
    }
    drop((out_tx, err_tx));
    let _ = done.send(true);
    if let Some(error) = failure {
        return Err(error);
    }
    if !closed {
        let reason = lost
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .unwrap_or_else(|| "channel ended without close".into());
        return Err(ExecError::ConnectionLost(reason));
    }
    Ok(exit)
}

fn signal_name_of(sig: &Sig) -> String {
    match sig {
        Sig::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

async fn collect(rx: Option<Lines>) -> Result<Vec<String>, ExecError> {
    let mut lines = Vec::new();
    if let Some(mut rx) = rx {
        while let Some(line) = rx.recv().await {
            lines.push(line?);
        }
    }
    Ok(lines)
}

impl CommandSession {
    pub async fn terminate(&self) {
        let _ = self.state.write.signal(Sig::TERM).await;
    }

    pub async fn kill(&self) {
        let _ = self.state.write.signal(Sig::KILL).await;
    }

    /// The Kotlin snapshot: true for every session that started, whatever happened since (A11)
    pub fn is_alive(&self) -> bool {
        self.alive_at_construction
    }

    /// Whether the channel is still open, which `is_alive` does not tell
    pub fn is_open(&self) -> bool {
        self.state.is_open()
    }

    /// Both streams and the exit code; the first failure wins, as with awaitAll
    pub async fn await_result(&mut self) -> Result<CommandResult, ExecError> {
        self.take_result().await
    }

    /// Start the same concurrent streams-and-exit await while retaining a close handle.
    pub fn start_await(&mut self) -> JoinHandle<Result<CommandResult, ExecError>> {
        tokio::spawn(self.take_result())
    }

    fn take_result(&mut self) -> impl Future<Output = Result<CommandResult, ExecError>> + Send + use<> {
        let (stdout, stderr) = (self.stdout.take(), self.stderr.take());
        let exit = self.exit.take();
        async move {
            let exit = async move {
                match exit {
                    Some(handle) => handle.await.unwrap_or_else(|e| {
                        Err(if e.is_cancelled() {
                            ExecError::Cancelled
                        } else {
                            ExecError::Other(e.to_string())
                        })
                    }),
                    None => Err(ExecError::Other("already awaited".into())),
                }
            };
            let (stdout, stderr, raw) = tokio::try_join!(collect(stdout), collect(stderr), exit)?;
            Ok(CommandResult {
                stdout,
                stderr,
                exit_code: raw.exit_code(),
                raw,
            })
        }
    }

    /// SshjCommandSession.close: TERM if open, `command.close()` (waits up to the connection timeout and fails after
    /// it, before the join), then `command.join()` without a bound, then the readers and the exit wait are cancelled.
    /// There is no KILL escalation. Once-only.
    pub async fn close(&mut self) -> Result<(), ExecError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if self.state.is_open() {
            self.terminate().await;
        }
        self.state.close().await?;
        self.state.join().await?;
        self.stdout = None;
        self.stderr = None;
        self.exit_abort.abort();
        Ok(())
    }
}
