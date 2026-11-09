//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/sshj/PatchedSSHClient.kt
//! and of the parts of sshj 0.39.0's `SSHClient`/`TransportImpl` it relies on: connect, authentication, `die`,
//! `isConnected` and `disconnect`, over the vendored russh client.
//!
//! russh needs a Tokio context: every client future and task runs on the vendor runtime (`VendorRuntime` spawns on
//! Tokio), never on core. Reply timeouts (30 s, sshj's `getTimeoutMs`) and the keep-alive go through `VendorClock`.

use super::channel::ExecChannel;
use super::config::{BridgeSink, SshComponents, bridge_russh_logs};
use super::error::*;
use super::keep_alive::{KeepAliveConnection, KeepAliveRunner};
use super::known_hosts::{HostKeyVerifier, key_type_of, md5_fingerprint};
use crate::cmd::{SessionScope, cancelled};
use marathon_logging::Level;
use marathon_core::runtime::{CancellationSignal, VendorClock, VendorRuntime, VendorTask};
use russh::{
    Channel, ChannelId, ChannelMsg, Disconnect, Error as RusshError,
    client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse},
    keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::sync::{oneshot, watch};

pub type SshFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SshFailure>> + Send + 'a>>;

/// sshj `Connection.getTimeoutMs()` / `Transport.getTimeoutMs()`.
pub const TIMEOUT_MILLIS: i64 = 30_000;
pub const TIMEOUT_TEXT: &str = "30000 MILLISECONDS";
/// sshj's default local window and maximum packet size (`ConnectionImpl`), also russh's.
pub const WINDOW_SIZE: u32 = 2_097_152;
pub const MAX_PACKET_SIZE: u32 = 32_768;

/// The `SSHClient` operations the factory drives; the Kotlin tests replace them with a mock (`connect(newClient, …)`).
pub trait SshClient: Send {
    type Keys: Send;
    fn connect<'a>(&'a mut self, addr: &'a str, port: u16) -> SshFuture<'a, ()>;
    fn auth_password<'a>(&'a mut self, username: &'a str, password: &'a str) -> SshFuture<'a, ()>;
    fn load_keys<'a>(&'a mut self, path: &'a Path) -> SshFuture<'a, Self::Keys>;
    fn auth_publickey<'a>(&'a mut self, username: &'a str, keys: Self::Keys) -> SshFuture<'a, ()>;
    fn close(&mut self) -> SshFuture<'_, ()>;
}

/// `None` when `millis` pass first. A cancelled clock never counts as expiry; the caller's own select handles it.
pub(crate) async fn within<T>(
    clock: &dyn VendorClock,
    cancellation: Arc<dyn CancellationSignal>,
    millis: i64,
    future: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        value = future => Some(value),
        _ = async {
            if clock.delay(millis, cancellation).await.is_err() {
                std::future::pending::<()>().await;
            }
        } => None,
    }
}

/// Transport state shared by the russh handler, the keep-alive and every channel.
pub struct ConnectionShared {
    pub loggers: SshComponents,
    death: Mutex<Option<SshFailure>>,
    dead: watch::Sender<bool>,
    received_disconnect: AtomicBool,
    closed_locally: AtomicBool,
    authenticated: AtomicBool,
    host_key_rejection: Mutex<Option<SshFailure>>,
    next_channel: AtomicU32,
    #[cfg(feature = "test-support")]
    peak_unread: std::sync::atomic::AtomicUsize,
}
impl ConnectionShared {
    pub(super) fn new(loggers: SshComponents) -> Arc<Self> {
        Arc::new(Self {
            loggers,
            death: Mutex::new(None),
            dead: watch::channel(false).0,
            received_disconnect: AtomicBool::new(false),
            closed_locally: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            host_key_rejection: Mutex::new(None),
            next_channel: AtomicU32::new(0),
            #[cfg(feature = "test-support")]
            peak_unread: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    /// The most unread bytes any stream of this connection has held, for tests that must reach the cap.
    #[cfg(feature = "test-support")]
    pub fn peak_unread(&self) -> usize {
        self.peak_unread.load(Ordering::SeqCst)
    }
    #[cfg(feature = "test-support")]
    pub(super) fn note_unread(&self, unread: usize) {
        self.peak_unread.fetch_max(unread, Ordering::SeqCst);
    }
    pub fn death(&self) -> Option<SshFailure> {
        self.death.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn is_dead(&self) -> bool {
        *self.dead.borrow()
    }
    pub async fn wait_dead(&self) {
        let mut rx = self.dead.subscribe();
        let _ = rx.wait_for(|dead| *dead).await;
    }
    /// D54: once the transport has died other than by our own `close()`, why it died. Never after our own close.
    pub async fn wait_lost(&self) -> SshFailure {
        self.wait_dead().await;
        if self.closed_locally.load(Ordering::SeqCst) {
            return std::future::pending().await;
        }
        self.channel_failure_cause()
    }
    /// `TransportImpl.die`: the first cause wins, is logged at ERROR and is delivered to every channel.
    pub fn die(&self, cause: SshFailure) -> bool {
        self.set_death(cause, true, false)
    }
    /// `TransportImpl.disconnect` suppresses the `Dying` line for a local close.
    fn set_death(&self, cause: SshFailure, log_cause: bool, closed_locally: bool) -> bool {
        {
            let mut death = self.death.lock().unwrap_or_else(|e| e.into_inner());
            if death.is_some() {
                return false;
            }
            // sshj logs before notifying promises. Keep the cause private too: a channel whose russh
            // queue closes concurrently must not retrieve it and log its wake-up ahead of this line.
            if log_cause {
                self.loggers.dying(&cause);
            }
            // Under the lock and before `dead`: nobody sees this death without knowing whose it was.
            self.closed_locally.store(closed_locally, Ordering::SeqCst);
            *death = Some(cause);
        }
        self.dead.send_replace(true);
        true
    }
    /// The error a channel or request receives when the transport is gone.
    pub fn channel_failure(&self) -> SshFailure {
        SshFailure::chain_connection(
            &self.death().unwrap_or_else(|| {
                SshFailure::transport(DisconnectReason::Unknown, "Broken transport; encountered EOF")
            }),
        )
    }
    /// One line on the executor's logger for what the failure a caller gets cannot say.
    pub(crate) fn log_executor_error(&self, message: String) {
        let logger = self
            .loggers
            .logging
            .component("marathon_apple::cmd::remote::ssh::sshj::client");
        self.loggers.logging.log(&logger, Level::ERROR, || message, None);
    }
    /// A dead transport rejects writes, as sshj's closed socket does.
    pub fn write_failure(&self) -> SshFailure {
        SshFailure::wrapping(TRANSPORT_EXCEPTION, SshFailure::new(SOCKET_EXCEPTION, "Socket closed"))
    }
    pub fn next_channel_number(&self) -> u32 {
        self.next_channel.fetch_add(1, Ordering::SeqCst)
    }
}

pub struct ClientHandler {
    shared: Arc<ConnectionShared>,
    verifier: Arc<dyn HostKeyVerifier>,
    addr: String,
    port: u16,
}
impl client::Handler for ClientHandler {
    type Error = RusshError;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        if self.verifier.verify(&self.addr, self.port, key) {
            return Ok(true);
        }
        let key_type = key_type_of(key).unwrap_or("unknown");
        let fingerprint = md5_fingerprint(key);
        let loggers = &self.shared.loggers;
        loggers.logging.log(
            &loggers.key_exchanger,
            Level::ERROR,
            || {
                format!(
                    "Disconnecting because none of the configured Host key verifiers ([{}]) could verify '{key_type}' host key with fingerprint {fingerprint} for {}:{}",
                    self.verifier.describe(),
                    self.addr,
                    self.port
                )
            },
            None,
        );
        *self.shared.host_key_rejection.lock().unwrap_or_else(|e| e.into_inner()) = Some(SshFailure::transport(
            DisconnectReason::HostKeyNotVerifiable,
            format!(
                "Could not verify `{key_type}` host key with fingerprint `{fingerprint}` for `{}` on port {}",
                self.addr, self.port
            ),
        ));
        Ok(false)
    }
    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                // TransportImpl.gotDisconnect throws TransportException(reason, message) into `die`.
                self.shared.received_disconnect.store(true, Ordering::SeqCst);
                self.shared.die(SshFailure::transport(
                    DisconnectReason::from_int(info.reason_code as u32),
                    info.message,
                ));
            }
            client::DisconnectReason::Error(error) => {
                let failure = self
                    .shared
                    .host_key_rejection
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .unwrap_or_else(|| transport_failure(&error));
                self.shared.die(failure);
            }
        }
        Ok(())
    }
}

/// sshj's view of a russh transport error (R5).
pub fn transport_failure(error: &RusshError) -> SshFailure {
    use std::io::ErrorKind;
    match error {
        RusshError::IO(io) => match io.kind() {
            ErrorKind::UnexpectedEof => {
                SshFailure::transport(DisconnectReason::Unknown, "Broken transport; encountered EOF")
            }
            _ => SshFailure::wrapping(TRANSPORT_EXCEPTION, io_failure(SOCKET_EXCEPTION, io)),
        },
        RusshError::Disconnect | RusshError::HUP | RusshError::SendError | RusshError::RecvError => {
            SshFailure::transport(DisconnectReason::Unknown, "Broken transport; encountered EOF")
        }
        RusshError::NoCommonAlgo { kind, ours, theirs } => {
            let what = match kind {
                russh::AlgorithmKind::Kex => "KeyExchangeAlgorithms",
                russh::AlgorithmKind::Key => "HostKeyAlgorithms",
                russh::AlgorithmKind::Cipher => "Client2ServerCipherAlgorithms",
                russh::AlgorithmKind::Mac => "Client2ServerMACAlgorithms",
                russh::AlgorithmKind::Compression => "Client2ServerCompressionAlgorithms",
            };
            SshFailure::no_common_algorithm(format!(
                "Unable to reach a settlement of {what}: [{}] and [{}]",
                ours.join(", "),
                theirs.join(", ")
            ))
        }
        other => SshFailure::transport(DisconnectReason::Unknown, other.to_string()),
    }
}

/// russh's `Disconnect` for an sshj reason code; UNKNOWN has none and goes out as BY_APPLICATION.
fn wire_reason(reason: DisconnectReason) -> Disconnect {
    Disconnect::try_from(reason.to_int()).unwrap_or(Disconnect::ByApplication)
}

/// Configuration of one client, as `createClient` sets it up.
#[derive(Clone)]
pub struct ClientConfig {
    pub keep_alive: KeepAliveRunner,
    pub verifier: Arc<dyn HostKeyVerifier>,
    pub loggers: SshComponents,
    pub runtime: Arc<dyn VendorRuntime>,
    pub clock: Arc<dyn VendorClock>,
}

/// A fresh `PatchedSSHClient(PerformanceDefaultConfig(verbose))` per connection attempt.
pub struct RusshClient {
    pub config: ClientConfig,
    handle: Option<Handle<ClientHandler>>,
    authenticated: Option<Arc<Handle<ClientHandler>>>,
    handle_ref: HandleRef,
    shared: Option<Arc<ConnectionShared>>,
    scope: Arc<SessionScope>,
    keep_alive: Option<Arc<dyn VendorTask>>,
}
impl RusshClient {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            handle: None,
            authenticated: None,
            handle_ref: HandleRef::default(),
            shared: None,
            scope: Arc::new(SessionScope::default()),
            keep_alive: None,
        }
    }
    pub fn russh_config(&self, addr: &str, port: u16) -> client::Config {
        // KeyExchanger builds its proposal with the verifier's known types first, otherwise in configured order.
        let known = self.config.verifier.find_existing_algorithms(addr, port);
        let mut keys: Vec<Algorithm> = russh::Preferred::DEFAULT.key.to_vec();
        keys.sort_by_key(|algorithm| !known.iter().any(|k| k == algorithm.as_str()));
        client::Config {
            // sshj's KeepAliveRunner replaces russh's keep-alive, which any received data resets (R7).
            keepalive_interval: None,
            inactivity_timeout: None,
            manual_window_adjust: true,
            window_size: WINDOW_SIZE,
            maximum_packet_size: MAX_PACKET_SIZE,
            preferred: russh::Preferred {
                key: keys.into(),
                ..russh::Preferred::DEFAULT
            },
            ..Default::default()
        }
    }
    fn cancellation(&self) -> Arc<dyn CancellationSignal> {
        self.scope.clone()
    }
    async fn reply<T>(&self, future: impl Future<Output = T>) -> Option<T> {
        within(self.config.clock.as_ref(), self.cancellation(), TIMEOUT_MILLIS, future).await
    }
    /// One `authenticate(username, method)` round: a USERAUTH_FAILURE is `false`, a transport error propagates.
    async fn auth_round(
        &mut self,
        future: impl FnOnce(
            &mut Handle<ClientHandler>,
        ) -> Pin<Box<dyn Future<Output = Result<bool, RusshError>> + Send + '_>>,
    ) -> Result<bool, SshFailure> {
        let shared = self.shared.clone().ok_or_else(SshFailure::not_connected)?;
        let clock = self.config.clock.clone();
        let cancellation = self.cancellation();
        let handle = self.handle.as_mut().ok_or_else(SshFailure::not_connected)?;
        match within(clock.as_ref(), cancellation, TIMEOUT_MILLIS, future(handle)).await {
            Some(Ok(outcome)) => Ok(outcome),
            Some(Err(_)) => {
                let failure = SshFailure::chain_user_auth(&shared.channel_failure_cause());
                shared.loggers.woke_to("authenticated", &failure);
                Err(failure)
            }
            None => Err(SshFailure::timeout_expired(USER_AUTH_EXCEPTION, TIMEOUT_TEXT)),
        }
    }
    /// `SSHClient.auth`: every method is tried in order; all failing is "Exhausted available authentication methods"
    /// with the last saved UserAuthException (a timeout, A15) as cause.
    async fn auth(&mut self, methods: Vec<AuthMethod>) -> Result<(), SshFailure> {
        let mut saved: Option<SshFailure> = None;
        for method in methods {
            let outcome = match method {
                AuthMethod::Password { user, password } => {
                    self.auth_round(move |handle| {
                        Box::pin(async move { handle.authenticate_password(user, password).await.map(|r| r.success()) })
                    })
                    .await
                }
                AuthMethod::KeyboardInteractive { user, password } => {
                    self.auth_round(move |handle| Box::pin(keyboard_interactive(handle, user, password)))
                        .await
                }
                AuthMethod::PublicKey { user, key } => {
                    self.auth_round(move |handle| {
                        Box::pin(async move {
                            let hash = handle.best_supported_rsa_hash().await?.flatten();
                            let key = PrivateKeyWithHashAlg::new(key, hash);
                            handle
                                .authenticate_publickey(user, key)
                                .await
                                .map(|r: AuthResult| r.success())
                        })
                    })
                    .await
                }
            };
            match outcome {
                Ok(true) => {
                    // Authentication is over: the handle becomes shared before the service counts as active.
                    if let Some(handle) = self.handle.take() {
                        let handle = Arc::new(handle);
                        self.handle_ref.set(&handle);
                        self.authenticated = Some(handle);
                    }
                    if let Some(shared) = &self.shared {
                        shared.authenticated.store(true, Ordering::SeqCst);
                    }
                    return Ok(());
                }
                Ok(false) => {}
                Err(failure) if failure.is_instance(USER_AUTH_EXCEPTION) && !self.is_dead() => saved = Some(failure),
                Err(failure) => return Err(failure),
            }
        }
        let mut exhausted = SshFailure::new(USER_AUTH_EXCEPTION, "Exhausted available authentication methods");
        exhausted.cause = saved.map(Box::new);
        Err(exhausted)
    }
    fn is_dead(&self) -> bool {
        self.shared.as_ref().is_some_and(|shared| shared.is_dead())
    }
    pub fn keep_alive_settings(&self) -> &KeepAliveRunner {
        &self.config.keep_alive
    }
    /// Hand the authenticated transport to an executor.
    pub fn into_connection(mut self) -> Result<SshConnection, SshFailure> {
        let handle = self.authenticated.take().ok_or_else(SshFailure::not_connected)?;
        let shared = self.shared.take().ok_or_else(SshFailure::not_connected)?;
        Ok(SshConnection {
            handle,
            handle_ref: self.handle_ref.clone(),
            shared,
            runtime: self.config.runtime.clone(),
            clock: self.config.clock.clone(),
            scope: self.scope.clone(),
            keep_alive: self.keep_alive.take(),
            _bridge: self
                .config
                .loggers
                .config
                .verbose
                .then(|| bridge_russh_logs(&self.config.loggers.logging)),
        })
    }
}
impl ConnectionShared {
    fn channel_failure_cause(&self) -> SshFailure {
        self.death()
            .unwrap_or_else(|| SshFailure::transport(DisconnectReason::Unknown, "Broken transport; encountered EOF"))
    }
}

enum AuthMethod {
    Password { user: String, password: String },
    KeyboardInteractive { user: String, password: String },
    PublicKey { user: String, key: Arc<PrivateKey> },
}

/// sshj `AuthKeyboardInteractive` with `PasswordResponseProvider`: every non-echoed prompt matching
/// `.*[pP]assword(?: for .*)?:\s?\z` gets the password, any other prompt an empty answer.
async fn keyboard_interactive(
    handle: &mut Handle<ClientHandler>,
    user: String,
    password: String,
) -> Result<bool, RusshError> {
    let mut response = handle.authenticate_keyboard_interactive_start(user, None).await?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts
                    .iter()
                    .map(|p| {
                        if !p.echo && password_prompt(&p.prompt) {
                            password.clone()
                        } else {
                            String::new()
                        }
                    })
                    .collect();
                response = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
}
pub fn password_prompt(prompt: &str) -> bool {
    let trimmed = prompt
        .strip_suffix(|c: char| c.is_ascii_whitespace() || c == '\u{b}')
        .unwrap_or(prompt);
    let Some(before_colon) = trimmed.strip_suffix(':') else {
        return false;
    };
    // `.*[pP]assword` either ends the text before the colon or is followed by ` for …`.
    before_colon.ends_with("assword") && before_colon[..before_colon.len() - 7].ends_with(['p', 'P'])
        || before_colon.match_indices(" for ").any(|(index, _)| {
            let head = &before_colon[..index];
            head.ends_with("assword") && head[..head.len() - 7].ends_with(['p', 'P'])
        })
}

impl SshClient for RusshClient {
    type Keys = String;
    fn connect<'a>(&'a mut self, addr: &'a str, port: u16) -> SshFuture<'a, ()> {
        Box::pin(async move {
            // InetAddress.getByName: the first resolved address only (approved P8); no connect timeout of ours.
            let address = match tokio::net::lookup_host((addr, port)).await {
                Ok(mut addresses) => addresses.next(),
                Err(error) => {
                    let message = error.to_string();
                    let message = message
                        .strip_prefix("failed to lookup address information: ")
                        .unwrap_or(&message);
                    return Err(SshFailure::new(UNKNOWN_HOST_EXCEPTION, format!("{addr}: {message}")));
                }
            }
            .ok_or_else(|| SshFailure::new(UNKNOWN_HOST_EXCEPTION, format!("{addr}: no address")))?;
            let stream = tokio::net::TcpStream::connect(address)
                .await
                .map_err(|e| io_failure(CONNECT_EXCEPTION, &e))?;
            let shared = ConnectionShared::new(self.config.loggers.clone());
            self.shared = Some(shared.clone());
            let handler = ClientHandler {
                shared: shared.clone(),
                verifier: self.config.verifier.clone(),
                addr: addr.into(),
                port,
            };
            let config = Arc::new(self.russh_config(addr, port));
            let result = self.reply(client::connect_stream(config, stream, handler)).await;
            let handle = match result {
                Some(Ok(handle)) => handle,
                Some(Err(error)) => {
                    let rejection = shared
                        .host_key_rejection
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let failure = rejection
                        .or_else(|| shared.death())
                        .unwrap_or_else(|| transport_failure(&error));
                    shared.die(failure.clone());
                    shared.loggers.woke_to("kex done", &failure);
                    return Err(failure);
                }
                None => return Err(SshFailure::timeout_expired(TRANSPORT_EXCEPTION, TIMEOUT_TEXT)),
            };
            // Kill the transport whenever a channel, the keep-alive or the peer declares it dead.
            let reaper_shared = shared.clone();
            let reaper = self.handle_ref.clone();
            self.scope.track(self.config.runtime.spawn(Box::pin(async move {
                reaper_shared.wait_dead().await;
                if !reaper_shared.received_disconnect.load(Ordering::SeqCst) {
                    let cause = reaper_shared.death().unwrap_or_default_failure();
                    let message = cause.message().unwrap_or_default();
                    let _ = reaper.disconnect(wire_reason(cause.reason), &message).await;
                }
                Ok(())
            })));
            // sshj starts the keep-alive thread on connect; it sends only once authentication made the
            // connection service active.
            if self.config.keep_alive.is_enabled() {
                let runner = self.config.keep_alive.clone();
                let connection: Arc<dyn KeepAliveConnection> = Arc::new(KeepAlivePort {
                    shared: shared.clone(),
                    handle: self.handle_ref.clone(),
                    runtime: self.config.runtime.clone(),
                });
                let clock = self.config.clock.clone();
                let scope = self.scope.clone();
                let transport = shared.clone();
                let task = self.config.runtime.spawn(Box::pin(async move {
                    // sshj interrupts the keep-alive thread when the transport dies.
                    tokio::select! {
                        biased;
                        _ = scope.cancelled() => {},
                        _ = transport.wait_dead() => {},
                        _ = runner.run(connection, clock.as_ref(), scope.clone()) => {},
                    }
                    Ok(())
                }));
                self.keep_alive = Some(task);
            }
            self.handle = Some(handle);
            Ok(())
        })
    }
    fn auth_password<'a>(&'a mut self, username: &'a str, password: &'a str) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.auth(vec![
                AuthMethod::Password {
                    user: username.into(),
                    password: password.into(),
                },
                AuthMethod::KeyboardInteractive {
                    user: username.into(),
                    password: password.into(),
                },
            ])
            .await
        })
    }
    fn load_keys<'a>(&'a mut self, path: &'a Path) -> SshFuture<'a, String> {
        Box::pin(async move {
            // KeyProviderUtil.detectKeyFileFormat reads the file eagerly; decoding waits for authentication.
            tokio::fs::read_to_string(path).await.map_err(|e| {
                let os = io_failure(FILE_NOT_FOUND_EXCEPTION, &e);
                SshFailure::new(
                    FILE_NOT_FOUND_EXCEPTION,
                    format!("{} ({})", path.display(), os.message().unwrap_or_default()),
                )
            })
        })
    }
    fn auth_publickey<'a>(&'a mut self, username: &'a str, keys: String) -> SshFuture<'a, ()> {
        Box::pin(async move {
            let key = match russh::keys::decode_secret_key(&keys, None) {
                Ok(key) => key,
                Err(error) => {
                    let mut exhausted =
                        SshFailure::new(USER_AUTH_EXCEPTION, "Exhausted available authentication methods");
                    exhausted.cause = Some(Box::new(SshFailure::wrapping(
                        USER_AUTH_EXCEPTION,
                        key_failure(&keys, &error),
                    )));
                    return Err(exhausted);
                }
            };
            self.auth(vec![AuthMethod::PublicKey {
                user: username.into(),
                key: Arc::new(key),
            }])
            .await
        })
    }
    fn close(&mut self) -> SshFuture<'_, ()> {
        Box::pin(async move {
            let keep_alive = self.keep_alive.take();
            if let Some(task) = &keep_alive {
                task.cancel(cancelled("Client closed"));
            }
            if let Some(handle) = self.handle.take() {
                disconnect_locally(self.shared.as_deref(), &handle).await;
            } else if let Some(handle) = self.authenticated.take() {
                disconnect_locally(self.shared.as_deref(), &handle).await;
            }
            self.scope.cancel(cancelled("Client closed"));
            // The scope's cancellation ends the keep-alive (cancel() does not abort a spawned task).
            if let Some(task) = keep_alive {
                let _ = task.join().await;
            }
            Ok(())
        })
    }
}

/// T13: the key-provider failures sshj reports for keys it cannot decrypt without a passphrase.
fn key_failure(text: &str, error: &russh::keys::Error) -> SshFailure {
    if text.trim_start().starts_with("PuTTY-User-Key-File-") {
        return SshFailure::new(IO_EXCEPTION, "Invalid passphrase");
    }
    match error {
        russh::keys::Error::KeyIsEncrypted if text.contains("BEGIN OPENSSH PRIVATE KEY") => SshFailure::new(
            KEY_DECRYPTION_FAILED_EXCEPTION,
            "Decryption of the key failed. A supplied passphrase may be incorrect.",
        )
        .caused_by(SshFailure::new(
            ENCRYPTION_EXCEPTION,
            "OpenSSH Private Key integer comparison failed",
        )),
        // BouncyCastle's PKCS#8 decryptor rejects the empty password itself; russh fails it as bad DER instead.
        _ if text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----") => SshFailure::new(
            KEY_DECRYPTION_FAILED_EXCEPTION,
            "Decryption of the key failed. A supplied passphrase may be incorrect.",
        )
        .caused_by(
            SshFailure::new(ENCRYPTION_EXCEPTION, "Reading Encrypted Private Key Failed").caused_by(
                SshFailure::new(PKCS_EXCEPTION, "unable to read encrypted data: password empty")
                    .caused_by(SshFailure::new(ILLEGAL_ARGUMENT_EXCEPTION, "password empty")),
            ),
        ),
        russh::keys::Error::KeyIsEncrypted => {
            SshFailure::new(PASSWORD_EXCEPTION, "Password is null, but a password is required")
        }
        other => SshFailure::new(IO_EXCEPTION, other.to_string()),
    }
}

trait DeathOrDefault {
    fn unwrap_or_default_failure(self) -> SshFailure;
}
impl DeathOrDefault for Option<SshFailure> {
    fn unwrap_or_default_failure(self) -> SshFailure {
        self.unwrap_or_else(|| SshFailure::transport(DisconnectReason::ByApplication, "Disconnected"))
    }
}

/// russh's `Handle` is not `Clone`. The executor owns it; the reaper, keep-alive and channels reach it weakly,
/// so a dropped executor still lets the session end.
#[derive(Clone, Default)]
pub struct HandleRef(Arc<OnceLock<Weak<Handle<ClientHandler>>>>);
impl HandleRef {
    fn get(&self) -> Result<Arc<Handle<ClientHandler>>, RusshError> {
        self.0.get().and_then(Weak::upgrade).ok_or(RusshError::SendError)
    }
    fn set(&self, handle: &Arc<Handle<ClientHandler>>) {
        let _ = self.0.set(Arc::downgrade(handle));
    }
    pub async fn disconnect(&self, reason: Disconnect, message: &str) -> Result<(), RusshError> {
        self.get()?.disconnect(reason, message, "").await
    }
    pub async fn ping(&self) -> Result<(), RusshError> {
        self.get()?.send_ping().await
    }
    pub async fn adjust_window(&self, id: ChannelId, bytes: u32) -> Result<(), RusshError> {
        self.get()?.adjust_window(id, bytes).await
    }
}

type Opened = Result<Channel<client::Msg>, RusshError>;

/// The reply of an open, as the caller receives it. Until the caller claims it, the open task still owns it: dropped
/// unclaimed, whether in the oneshot or in a caller's future that was dropped before it was polled again, it goes back to
/// the task, which closes a channel as it does one that came after the caller had gone. A refusal carries its own text, so
/// dropping one leaves nothing behind.
struct OpenReply {
    opened: Option<Opened>,
    unclaimed: Option<oneshot::Sender<Opened>>,
}
impl OpenReply {
    /// From here the reply is the caller's: the task ends without touching it.
    fn claim(mut self) -> Opened {
        self.unclaimed = None;
        self.opened.take().unwrap_or(Err(RusshError::Disconnect))
    }
}
impl Drop for OpenReply {
    fn drop(&mut self) {
        if let (Some(opened), Some(unclaimed)) = (self.opened.take(), self.unclaimed.take()) {
            let _ = unclaimed.send(opened);
        }
    }
}

/// One channel open. Kotlin's `open()` gives up after sshj's 30 s and the channel stays attached, so a late confirmation
/// opens a channel that nobody ever closes, in a `MaxSessions` slot (U157). A CLOSE needs the peer's channel number, which only
/// the confirmation brings, so nothing can be sent before it: when no caller claims the reply, this waits for it and
/// closes what it opened. The reply stays this task's until the caller claims it (x38): a caller can go away after the reply
/// reached its oneshot. A refusal's text comes with the refusal, from russh's channel to this opener (vendor patch 4): a
/// queue shared by the connection let concurrent or abandoned refusals take each other's text. Holding the handle keeps the
/// transport up, but only until the reply, the transport's death or the client's own close.
async fn open_session(
    handle: Arc<Handle<ClientHandler>>,
    shared: Arc<ConnectionShared>,
    reply: oneshot::Sender<OpenReply>,
) -> marathon_core::error::VendorResult<()> {
    let opened = tokio::select! {
        biased;
        opened = handle.channel_open_session_described() => opened,
        _ = shared.wait_dead() => return Ok(()),
    };
    drop(handle);
    let (unclaimed, returned) = oneshot::channel();
    // A failed send drops the reply at once, which returns it below.
    let _ = reply.send(OpenReply {
        opened: Some(opened),
        unclaimed: Some(unclaimed),
    });
    let returned = tokio::select! {
        biased;
        _ = shared.wait_dead() => return Ok(()),
        returned = returned => returned,
    };
    match returned {
        Err(_) | Ok(Err(_)) => {}
        Ok(Ok(channel)) => {
            let (mut read, write) = channel.split();
            let _ = write.close().await;
            // As the pump does for a dropped channel, so that russh's queue never holds up the connection.
            loop {
                let message = tokio::select! {
                    biased;
                    _ = shared.wait_dead() => None,
                    message = read.wait() => message,
                };
                if matches!(message, None | Some(ChannelMsg::Close)) {
                    break;
                }
            }
        }
    }
    Ok(())
}

struct KeepAlivePort {
    shared: Arc<ConnectionShared>,
    handle: HandleRef,
    runtime: Arc<dyn VendorRuntime>,
}
impl KeepAliveConnection for KeepAlivePort {
    fn is_running(&self) -> bool {
        !self.shared.is_dead()
    }
    fn service_active(&self) -> bool {
        self.shared.authenticated.load(Ordering::SeqCst)
    }
    fn send_keep_alive(&self) -> Arc<AtomicBool> {
        // `keepalive@openssh.com` with want-reply; REQUEST_SUCCESS and REQUEST_FAILURE both fulfil it.
        let answered = Arc::new(AtomicBool::new(false));
        let fulfil = answered.clone();
        let handle = self.handle.clone();
        let shared = self.shared.clone();
        self.runtime.spawn(Box::pin(async move {
            tokio::select! {
                biased;
                _ = shared.wait_dead() => {},
                result = handle.ping() => if result.is_ok() && !shared.is_dead() {
                    fulfil.store(true, Ordering::SeqCst);
                },
            }
            Ok(())
        }));
        answered
    }
    fn die(&self, cause: SshFailure) {
        self.shared.die(cause);
    }
}

/// `TransportImpl.disconnect(BY_APPLICATION, "")`: channels get TransportException[BY_APPLICATION]("Disconnected").
async fn disconnect_locally(shared: Option<&ConnectionShared>, handle: &Handle<ClientHandler>) {
    if let Some(shared) = shared {
        // D36: disposal also closes a connection the worker already dropped, and a task still running on that device
        // must keep seeing a lost connection. russh closes the handle before it reports the death, so a transport
        // whose death is still on its way counts as lost too.
        shared.set_death(
            SshFailure::transport(DisconnectReason::ByApplication, "Disconnected"),
            false,
            !handle.is_closed(),
        );
        shared.received_disconnect.store(true, Ordering::SeqCst);
    }
    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
}

/// The authenticated client an executor owns.
pub struct SshConnection {
    handle: Arc<Handle<ClientHandler>>,
    handle_ref: HandleRef,
    pub shared: Arc<ConnectionShared>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub clock: Arc<dyn VendorClock>,
    scope: Arc<SessionScope>,
    keep_alive: Option<Arc<dyn VendorTask>>,
    _bridge: Option<Arc<BridgeSink>>,
}
impl SshConnection {
    /// `SSHClient.isConnected`: the transport is alive.
    pub fn is_connected(&self) -> bool {
        !self.shared.is_dead() && !self.handle.is_closed()
    }
    pub fn handle(&self) -> HandleRef {
        self.handle_ref.clone()
    }
    pub fn scope(&self) -> Arc<SessionScope> {
        self.scope.clone()
    }
    /// `PatchedSSHClient.startSession`: the checks, then a session channel opened under sshj's 30 s reply timeout.
    /// D36: a transport that died is a connection failure (A17); only our own `close()` leaves the state error.
    pub async fn start_session(&self) -> Result<Arc<ExecChannel>, SshFailure> {
        if !self.is_connected() {
            return Err(if self.shared.closed_locally.load(Ordering::SeqCst) {
                SshFailure::not_connected()
            } else {
                SshFailure::connection_gone(&self.shared.channel_failure_cause())
            });
        }
        if !self.shared.authenticated.load(Ordering::SeqCst) {
            return Err(SshFailure::new(ILLEGAL_STATE_EXCEPTION, "Not authenticated"));
        }
        let number = self.shared.next_channel_number();
        let promise = format!("chan#{number} / open");
        // U157: in a task of its own, because the open must outlive this wait to deal with a reply that comes after it.
        let (reply, mut replied) = oneshot::channel();
        self.runtime
            .spawn(Box::pin(open_session(self.handle.clone(), self.shared.clone(), reply)));
        let opened = tokio::select! {
            biased;
            opened = within(self.clock.as_ref(), self.scope.clone(), TIMEOUT_MILLIS, &mut replied) => opened.map(Result::ok),
            _ = self.shared.wait_dead() => Some(None),
        };
        // From here the task sees that nobody is waiting. A reply that got in just before this is still the caller's.
        replied.close();
        let opened = opened
            .or_else(|| replied.try_recv().ok().map(Some))
            .map(|reply| reply.map_or(Err(RusshError::Disconnect), OpenReply::claim));
        let channel = match opened {
            Some(Ok(channel)) => channel,
            Some(Err(RusshError::ChannelOpenRefused { description, .. })) => {
                let failure = SshFailure::new(OPEN_FAIL_EXCEPTION, description);
                self.shared.loggers.woke_to(&promise, &failure);
                return Err(failure);
            }
            Some(Err(_)) => {
                let failure = self.shared.channel_failure();
                self.shared.loggers.woke_to(&promise, &failure);
                return Err(failure);
            }
            None => {
                self.shared.log_executor_error(format!(
                    "No reply to opening chan#{number} in 30 s. If the worker answers later, that channel will be closed then"
                ));
                return Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT));
            }
        };
        Ok(ExecChannel::start(number, channel, self))
    }
    /// `SSHClient.close()`; disconnect failures are the caller's to log.
    pub async fn close(&self) -> Result<(), SshFailure> {
        if let Some(task) = &self.keep_alive {
            task.cancel(cancelled("Client closed"));
        }
        disconnect_locally(Some(&self.shared), &self.handle).await;
        self.scope.cancel(cancelled("Client closed"));
        // The scope's cancellation ends the keep-alive (cancel() does not abort a spawned task).
        if let Some(task) = &self.keep_alive {
            let _ = task.join().await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{PerformanceDefaultConfig, SshLogging};
    use super::*;
    use marathon_logging::LogSettings;
    use std::task::{Context, Waker};

    fn ready(future: impl Future<Output = ()>) -> bool {
        std::pin::pin!(future)
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_ready()
    }

    #[test]
    fn transport_death_is_logged_before_waiters_are_notified() {
        let connection: Arc<OnceLock<Weak<ConnectionShared>>> = Arc::default();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let watched = connection.clone();
        let records = observed.clone();
        let logging = SshLogging::new(
            Arc::new(Mutex::new(LogSettings::default())),
            Arc::new(move |logger, level, message, _| {
                let shared = watched.get().unwrap().upgrade().unwrap();
                // Inspect at sink entry so an early notification fails without depending on scheduling.
                records.lock().unwrap().push((
                    logger.name().to_owned(),
                    level,
                    message,
                    shared.is_dead(),
                    ready(shared.wait_dead()),
                ));
            }),
        );
        let shared = ConnectionShared::new(PerformanceDefaultConfig::new(false, logging).loggers());
        connection.set(Arc::downgrade(&shared)).unwrap();
        let waiter = shared.wait_dead();
        tokio::pin!(waiter);
        assert!(!ready(waiter.as_mut()));
        let cause = SshFailure::transport(DisconnectReason::ByApplication, "fixture disconnect 11");

        assert!(shared.die(cause.clone()));

        assert_eq!(
            *observed.lock().unwrap(),
            vec![(
                "marathon_apple::cmd::remote::ssh::sshj::wire".to_owned(),
                Level::ERROR,
                "Dying because - fixture disconnect 11".to_owned(),
                false,
                false,
            )]
        );
        assert!(ready(waiter));
        assert_eq!(shared.death(), Some(cause.clone()));
        assert!(!shared.die(SshFailure::transport(DisconnectReason::Unknown, "later failure")));
        assert_eq!(shared.death(), Some(cause));
        assert_eq!(observed.lock().unwrap().len(), 1);
    }
}
