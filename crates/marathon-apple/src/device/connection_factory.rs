//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/ConnectionFactory.kt

use marathon_logging::{Component, Level};

use crate::{
    cmd::{
        CommandExecutor, FileBridge, blocking, failure,
        local::{JvmFileBridge, KotlinProcessCommandExecutor},
        remote::{
            rsync::{RsyncFileBridge, RsyncTarget},
            ssh::sshj::{
                SshFailure, SshjCommandExecutorFactory,
                config::SshLogging,
                error::{ILLEGAL_ARGUMENT_EXCEPTION, ILLEGAL_STATE_EXCEPTION, UNKNOWN_HOST_EXCEPTION},
                known_hosts::{HostKeyVerifier, OpenSshKnownHosts},
            },
        },
        retained_tasks::RetainedTasks,
    },
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use jcompat::jtime::Duration;
use marathon_config::{
    devices::Transport,
    vendor::{RsyncConfiguration, SshAuthentication, SshConfiguration},
};
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{VendorClock, VendorRuntime},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::{
    collections::HashMap,
    future::Future,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::sync::{Semaphore, oneshot, watch};

pub type Executor = Arc<dyn CommandExecutor>;
pub type Bridge = Arc<dyn FileBridge>;
pub type ConnectionResult<T> = Result<T, ConnectionError>;
pub type ConnectionFuture<'a, T> = Pin<Box<dyn Future<Output = ConnectionResult<T>> + Send + 'a>>;
pub type PreparedConnection = Box<dyn FnOnce() -> ConnectionFuture<'static, Executor> + Send>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionError {
    Device(DeviceFailureException),
    Ssh(SshFailure),
    Vendor(VendorError),
}
impl ConnectionError {
    pub fn into_vendor(self) -> VendorError {
        match self {
            Self::Device(e) => VendorError::Other(e.to_failure()),
            Self::Ssh(e) => e.into_vendor(),
            Self::Vendor(e) => e,
        }
    }
}
impl From<VendorError> for ConnectionError {
    fn from(e: VendorError) -> Self {
        Self::Vendor(e)
    }
}
impl From<SshFailure> for ConnectionError {
    fn from(e: SshFailure) -> Self {
        Self::Ssh(e)
    }
}
fn configuration(message: impl AsRef<str>) -> ConnectionError {
    ConnectionError::Vendor(VendorError::Other(failure(
        FailureKind::Config(marathon_error::ConfigKind::Rejected),
        message,
    )))
}
fn unreachable(message: Option<String>, cause: Option<SshFailure>) -> ConnectionError {
    ConnectionError::Device(DeviceFailureException::new(
        DeviceFailureReason::UnreachableHost,
        message,
        cause,
    ))
}
/// Preparation runs before the fair handshake permit; only the returned connect's I/O errors are wrapped.
pub trait Connector: Send + Sync {
    fn prepare(&self, transport: Transport) -> ConnectionFuture<'_, PreparedConnection>;
    fn waiting(&self, _transport: &Transport) {}
    fn connected(&self, executor: Executor) -> ConnectionResult<bool> {
        Ok(executor.connected())
    }
}
pub trait AddressProbe: Send + Sync {
    fn check(&self, transport: Transport, timeout: Duration) -> ConnectionFuture<'_, ()>;
}
pub const DEFAULT_MAX_CONCURRENT_CONNECTS: i32 = 8;
struct Lease {
    executor: Executor,
    shared_by: Option<Transport>,
    users: usize,
}
type Attempt = watch::Sender<Option<ConnectionResult<()>>>;
#[derive(Default)]
struct State {
    leases: HashMap<usize, Lease>,
    shared: HashMap<Transport, Executor>,
    pending: HashMap<Transport, Attempt>,
}
fn identity(executor: &Executor) -> usize {
    Arc::as_ptr(executor) as *const () as usize
}

pub struct ConnectionFactory {
    runtime: Arc<dyn VendorRuntime>,
    clock: Arc<dyn VendorClock>,
    logging: SshLogging,
    logger: Component,
    debug: bool,
    ssh: SshConfiguration,
    rsync: RsyncConfiguration,
    timeout: Duration,
    connector: Arc<dyn Connector>,
    probe: Arc<dyn AddressProbe>,
    permits: Semaphore,
    state: Mutex<State>,
    bridges: Mutex<HashMap<RsyncTarget, Arc<RsyncFileBridge>>>,
    tasks: RetainedTasks,
}
impl ConnectionFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        debug: bool,
        ssh: SshConfiguration,
        rsync: RsyncConfiguration,
        timeout: Duration,
        runtime: Arc<dyn VendorRuntime>,
        clock: Arc<dyn VendorClock>,
        logging: SshLogging,
    ) -> ConnectionResult<Arc<Self>> {
        let connector = Arc::new(ProductionConnector {
            ssh: ssh.clone(),
            runtime: runtime.clone(),
            clock: clock.clone(),
            logging: logging.clone(),
        });
        let probe = Arc::new(EchoAddressProbe {
            runtime: runtime.clone(),
        });
        Self::with_seams(
            debug,
            ssh,
            rsync,
            timeout,
            DEFAULT_MAX_CONCURRENT_CONNECTS,
            runtime,
            clock,
            logging,
            connector,
            probe,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn with_seams(
        debug: bool,
        ssh: SshConfiguration,
        rsync: RsyncConfiguration,
        timeout: Duration,
        max_concurrent_connects: i32,
        runtime: Arc<dyn VendorRuntime>,
        clock: Arc<dyn VendorClock>,
        logging: SshLogging,
        connector: Arc<dyn Connector>,
        probe: Arc<dyn AddressProbe>,
    ) -> ConnectionResult<Arc<Self>> {
        if max_concurrent_connects <= 0 {
            return Err(SshFailure::new(
                ILLEGAL_ARGUMENT_EXCEPTION,
                format!("maxConcurrentConnects should be positive, got {max_concurrent_connects}"),
            )
            .into());
        }
        let logger = logging.component("marathon_apple::device::connection_factory");
        Ok(Arc::new(Self {
            runtime,
            clock,
            logging,
            logger,
            debug,
            ssh,
            rsync,
            timeout,
            connector,
            probe,
            permits: Semaphore::new(max_concurrent_connects as usize),
            state: Mutex::new(State::default()),
            bridges: Mutex::new(HashMap::new()),
            tasks: RetainedTasks::default(),
        }))
    }
    /// Resolve authentication only for diagnostics; transport keys and connection inputs stay unchanged.
    pub fn transport_log_text(&self, transport: &Transport, local_text: &str) -> String {
        transport_log_text(transport, self.ssh.authentication.as_ref(), local_text)
    }
    /// Every acquire belongs to a retained vendor task. A dropped/cancelled awaiter cannot strand a lease or permit.
    pub async fn create_or_throw(
        self: &Arc<Self>,
        transport: Transport,
        context: VendorContext,
    ) -> ConnectionResult<(Executor, Bridge)> {
        let (executor, bridge) = self.create_owned(transport, context, false).await?;
        executor
            .map(|executor| (executor, bridge))
            .ok_or_else(|| SshFailure::new(ILLEGAL_STATE_EXCEPTION, "missing executor").into())
    }
    pub async fn create(
        self: &Arc<Self>,
        transport: Transport,
        context: VendorContext,
    ) -> ConnectionResult<(Option<Executor>, Bridge)> {
        self.create_owned(transport, context, true).await
    }
    pub async fn connect_worker(
        self: &Arc<Self>,
        transport: Transport,
        context: VendorContext,
    ) -> ConnectionResult<Executor> {
        self.create_or_throw(transport, context).await.map(|pair| pair.0)
    }
    async fn create_owned(
        self: &Arc<Self>,
        transport: Transport,
        context: VendorContext,
        catch_device: bool,
    ) -> ConnectionResult<(Option<Executor>, Bridge)> {
        if let Some(reason) = context.cancellation.reason() {
            return Err(VendorError::Cancelled(reason).into());
        }
        let factory = self.clone();
        let (tx, rx) = oneshot::channel();
        self.tasks.spawn(self.runtime.as_ref(), async move {
            let result = factory.create_inner(transport.clone(), catch_device).await;
            let undelivered = result.as_ref().ok().and_then(|(executor, _)| executor.clone());
            let (ack_tx, ack_rx) = oneshot::channel();
            let delivered = tx.send((result, ack_tx)).is_ok() && ack_rx.await.is_ok();
            if !delivered && let Some(executor) = undelivered {
                factory.dispose_inner(executor).await?;
            }
            Ok(())
        });
        tokio::select! {
            biased;
            result = rx => {
                let (result, ack) = result.map_err(|e| ConnectionError::Ssh(SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string())))?;
                let _ = ack.send(());
                result
            },
            reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason).into()),
        }
    }
    async fn create_inner(
        &self,
        transport: Transport,
        catch_device: bool,
    ) -> ConnectionResult<(Option<Executor>, Bridge)> {
        if transport == Transport::Local {
            let (executor, bridge) = self.create_local();
            self.register(executor.clone(), None);
            return Ok((Some(executor), bridge));
        }
        let bridge = self.file_bridge(&transport).await?;
        match self.acquire(transport.clone()).await {
            Ok(executor) => Ok((Some(executor), bridge)),
            Err(ConnectionError::Device(error)) if catch_device => {
                if self.logging.enabled(&self.logger, Level::ERROR) {
                    (self.logging.sink)(
                        &self.logger,
                        Level::ERROR,
                        format!("Failed to initialize connection to {}", transport.id().to_owned()),
                        Some(error.to_failure()),
                    );
                }
                Ok((None, bridge))
            }
            Err(error) => Err(error),
        }
    }
    /// Direct local construction is deliberately unregistered, as in Kotlin.
    pub fn create_local(&self) -> (Executor, Bridge) {
        let logging = self.logging.clone();
        let executor = KotlinProcessCommandExecutor::new(
            self.runtime.clone(),
            self.logging.registry.clone(),
            move |logger, message| {
                if logging.enabled(logger, Level::ERROR) {
                    (logging.sink)(logger, Level::ERROR, message, None);
                }
            },
            false,
            64,
        );
        (
            Arc::new(executor),
            Arc::new(JvmFileBridge::new(self.runtime.clone(), true)),
        )
    }
    fn register(&self, executor: Executor, shared_by: Option<Transport>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(transport) = &shared_by {
            state.shared.insert(transport.clone(), executor.clone());
        }
        state.leases.insert(
            identity(&executor),
            Lease {
                executor,
                shared_by,
                users: 1,
            },
        );
    }
    async fn acquire(&self, transport: Transport) -> ConnectionResult<Executor> {
        if !self.ssh.share_worker_connection {
            let executor = self.connect(transport).await?;
            self.register(executor.clone(), None);
            return Ok(executor);
        }
        loop {
            let current = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let executor = state.shared.get(&transport).cloned();
                if let Some(executor) = &executor
                    && let Some(lease) = state.leases.get_mut(&identity(executor))
                {
                    lease.users += 1;
                }
                executor
            };
            if let Some(executor) = current {
                let check = executor.clone();
                let connector = self.connector.clone();
                let (tx, rx) = oneshot::channel();
                let task = self.runtime.spawn_blocking(Box::new(move || {
                    let _ = tx.send(connector.connected(check));
                    Ok(())
                }));
                task.join().await?;
                let alive = rx
                    .await
                    .map_err(|e| SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string()))?;
                match alive {
                    Ok(true) => return Ok(executor),
                    Ok(false) => {
                        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        if state
                            .shared
                            .get(&transport)
                            .is_some_and(|current| Arc::ptr_eq(current, &executor))
                        {
                            state.shared.remove(&transport);
                        }
                    }
                    Err(error) => {
                        self.dispose_inner(executor).await?;
                        return Err(error);
                    }
                }
                self.dispose_inner(executor).await?;
                continue;
            }
            let attempt = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.shared.contains_key(&transport) {
                    continue;
                }
                if let Some(attempt) = state.pending.get(&transport) {
                    (attempt.clone(), false)
                } else {
                    let (attempt, _) = watch::channel(None);
                    state.pending.insert(transport.clone(), attempt.clone());
                    (attempt, true)
                }
            };
            if attempt.1 {
                let result = self.connect(transport.clone()).await;
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.pending.remove(&transport);
                    if let Ok(executor) = &result {
                        state.shared.insert(transport.clone(), executor.clone());
                        state.leases.insert(
                            identity(executor),
                            Lease {
                                executor: executor.clone(),
                                shared_by: Some(transport.clone()),
                                users: 1,
                            },
                        );
                    }
                }
                attempt
                    .0
                    .send_replace(Some(result.as_ref().map(|_| ()).map_err(Clone::clone)));
                return result;
            }
            let mut rx = attempt.0.subscribe();
            self.connector.waiting(&transport);
            loop {
                if let Some(result) = rx.borrow().clone() {
                    result?;
                    break;
                }
                rx.changed()
                    .await
                    .map_err(|e| SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string()))?;
            }
        }
    }
    async fn connect(&self, transport: Transport) -> ConnectionResult<Executor> {
        self.probe.check(transport.clone(), self.timeout).await?;
        let connect = self.connector.prepare(transport).await?;
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|e| SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string()))?;
        match connect().await {
            Err(ConnectionError::Ssh(error)) if error.is_io() => Err(unreachable(None, Some(error))),
            result => result,
        }
    }
    pub async fn dispose(self: &Arc<Self>, executor: Executor) -> VendorResult<()> {
        let factory = self.clone();
        let task = self.tasks.spawn(
            self.runtime.as_ref(),
            async move { factory.dispose_inner(executor).await },
        );
        task.join().await
    }
    async fn dispose_inner(&self, executor: Executor) -> VendorResult<()> {
        let released = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            match state.leases.get_mut(&identity(&executor)) {
                None => None,
                Some(lease) if lease.users > 1 => {
                    lease.users -= 1;
                    return Ok(());
                }
                Some(_) => {
                    let lease = state.leases.remove(&identity(&executor));
                    if let Some(Lease {
                        shared_by: Some(transport),
                        ..
                    }) = &lease
                        && state
                            .shared
                            .get(transport)
                            .is_some_and(|current| Arc::ptr_eq(current, &executor))
                    {
                        state.shared.remove(transport);
                    }
                    lease
                }
            }
        };
        let Some(lease) = released else {
            self.logging.log(
                &self.logger,
                Level::WARN,
                || {
                    format!(
                        "Command executor for {} is not in use, ignoring dispose",
                        executor.host().id
                    )
                },
                None,
            );
            return Ok(());
        };
        self.logging.log(
            &self.logger,
            Level::DEBUG,
            || format!("Disposing of command executor for {}", executor.host().id),
            None,
        );
        lease.executor.close().await
    }
    async fn file_bridge(&self, transport: &Transport) -> ConnectionResult<Bridge> {
        let Transport::Ssh {
            addr,
            port,
            authentication,
            ..
        } = transport
        else {
            unreachable!()
        };
        let target = RsyncTarget {
            addr: addr.clone(),
            port: *port,
        };
        if let Some(bridge) = self
            .bridges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&target)
            .cloned()
        {
            return Ok(bridge);
        }
        let bridge = Arc::new(
            RsyncFileBridge::new(
                target.clone(),
                self.debug,
                self.ssh.clone(),
                self.rsync.clone(),
                authentication.clone().or_else(|| self.ssh.authentication.clone()),
                self.runtime.clone(),
                self.clock.clone(),
                self.logging.clone(),
            )
            .await?,
        );
        Ok(self
            .bridges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(target)
            .or_insert(bridge)
            .clone())
    }
    #[doc(hidden)]
    pub fn retained_tasks(&self) -> usize {
        self.tasks.len()
    }
    pub async fn join_pending(&self) -> VendorResult<()> {
        let mut result = Ok(());
        loop {
            let tasks = self.tasks.take();
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                // A failed close must not leave later owned work unjoined.
                result = result.and(task.join().await);
            }
        }
        let bridges: Vec<_> = self
            .bridges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for bridge in bridges {
            result = result.and(bridge.join_pending().await);
        }
        result
    }
}
struct ProductionConnector {
    ssh: SshConfiguration,
    runtime: Arc<dyn VendorRuntime>,
    clock: Arc<dyn VendorClock>,
    logging: SshLogging,
}
impl Connector for ProductionConnector {
    fn prepare(&self, transport: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        Box::pin(async move {
            let Transport::Ssh {
                addr,
                port,
                authentication,
                ..
            } = transport
            else {
                return Err(configuration("SSH transport required"));
            };
            let auth = authentication
                .or_else(|| self.ssh.authentication.clone())
                .ok_or_else(|| configuration(format!("no ssh auth provided for {}:{port}", addr.to_owned())))?;
            let auth = match auth {
                SshAuthentication::Password { username, password } => {
                    crate::cmd::remote::ssh::sshj::SshAuthentication::Password {
                        username: username.to_owned(),
                        password: password.to_owned(),
                    }
                }
                SshAuthentication::PublicKey { username, key } => {
                    crate::cmd::remote::ssh::sshj::SshAuthentication::PublicKey {
                        username: username.to_owned(),
                        key,
                    }
                }
            };
            let verifier: Option<Arc<dyn HostKeyVerifier>> = if let Some(path) = &self.ssh.known_hosts_path {
                let path = path.clone();
                let logging = self.logging.clone();
                Some(
                    blocking(self.runtime.as_ref(), move || {
                        OpenSshKnownHosts::load(&path, logging)
                            .map(|v| Arc::new(v) as Arc<dyn HostKeyVerifier>)
                            .map_err(SshFailure::into_vendor)
                    })
                    .await?,
                )
            } else {
                None
            };
            let mut factory =
                SshjCommandExecutorFactory::new(self.logging.clone(), self.runtime.clone(), self.clock.clone());
            factory.keep_alive_interval = self.ssh.keep_alive_interval;
            factory.keep_alive_max_count = self.ssh.keep_alive_max_count;
            let debug = self.ssh.debug;
            // Validation remains outside the connector's IOException catch boundary.
            let port = u16::try_from(port)
                .map_err(|_| SshFailure::new(ILLEGAL_ARGUMENT_EXCEPTION, format!("port out of range:{port}")))?;
            Ok(Box::new(move || {
                Box::pin(async move {
                    factory
                        .connect(&addr.to_owned(), port, auth, verifier, debug)
                        .await
                        .map(|e| Arc::new(e) as Executor)
                        .map_err(Into::into)
                }) as ConnectionFuture<'static, Executor>
            }) as PreparedConnection)
        })
    }
}
/// The approved unprivileged JDK path: resolve even when disabled; echo-port refusal means reachable.
pub struct EchoAddressProbe {
    pub runtime: Arc<dyn VendorRuntime>,
}
impl AddressProbe for EchoAddressProbe {
    fn check(&self, transport: Transport, timeout: Duration) -> ConnectionFuture<'_, ()> {
        Box::pin(async move {
            let runtime = self.runtime.clone();
            let (tx, rx) = oneshot::channel();
            let task = runtime.spawn_blocking(Box::new(move || {
                let _ = tx.send(check_address(transport, timeout));
                Ok(())
            }));
            task.join().await?;
            rx.await
                .map_err(|e| SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string()))?
        })
    }
}
/// Diagnostic transport text omits passwords, including on reachability failures.
pub fn transport_text(transport: &Transport, local_text: &str) -> String {
    match transport {
        Transport::Local => local_text.to_owned(),
        Transport::Ssh {
            addr,
            port,
            authentication,
            check_reachability,
        } => {
            let auth = match authentication {
                None => "none".into(),
                Some(SshAuthentication::Password { username, .. }) => format!("password (user='{username}')"),
                Some(SshAuthentication::PublicKey { username, key }) => {
                    format!("public key (user={username}, key={})", key.display())
                }
            };
            format!("ssh {addr}:{port} (authentication={auth}, check_reachability={check_reachability})")
        }
    }
}

/// The same transport-before-vendor precedence used by ProductionConnector::prepare.
pub fn transport_log_text(transport: &Transport, default_auth: Option<&SshAuthentication>, local_text: &str) -> String {
    let mut effective = transport.clone();
    if let Transport::Ssh { authentication, .. } = &mut effective
        && authentication.is_none()
    {
        *authentication = default_auth.cloned();
    }
    transport_text(&effective, local_text)
}

fn check_address(transport: Transport, timeout: Duration) -> ConnectionResult<()> {
    let Transport::Ssh {
        addr,
        check_reachability,
        ..
    } = &transport
    else {
        return Ok(());
    };
    let host = addr.to_owned();
    let host = if host.is_empty() { "127.0.0.1" } else { &host };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .filter(|h| h.contains(':'))
        .unwrap_or(host);
    let resolved = (host, 7).to_socket_addrs();
    let address = resolved
        .as_ref()
        .ok()
        .and_then(|addresses| {
            let addresses: Vec<_> = addresses.clone().collect();
            // InetAddress's default preferIPv6Addresses=false puts IPv4 before IPv6.
            addresses
                .iter()
                .find(|address| address.is_ipv4())
                .or_else(|| addresses.first())
                .copied()
        })
        .ok_or_else(|| {
            // NET_ThrowUnknownHostExceptionWithGaiError: "<host>: <gai_strerror>".
            let message = match &resolved {
                Err(error) => {
                    let error = error.to_string();
                    let gai = error
                        .strip_prefix("failed to lookup address information: ")
                        .unwrap_or(&error);
                    format!("{}: {gai}", addr.to_owned())
                }
                Ok(_) => addr.to_owned(),
            };
            unreachable(
                Some(format!("Error resolving host {}", transport_text(&transport, "Local"))),
                Some(SshFailure::new(UNKNOWN_HOST_EXCEPTION, message)),
            )
        })?;
    if !check_reachability {
        return Ok(());
    }
    let timeout = timeout
        .to_millis()
        .filter(|ms| (0..=i32::MAX as i64).contains(ms))
        .ok_or_else(|| configuration("reachability timeout must be between 0 and 2147483647 ms"))?;
    match tcp_echo(address, timeout as i32) {
        Ok(true) => Ok(()),
        Ok(false) => Err(unreachable(
            Some(format!(
                "{} is not reachable within {timeout} ms",
                transport_text(&transport, "Local")
            )),
            None,
        )),
        Err(exception) => Err(unreachable(
            Some(format!(
                "Error checking reachability of {}: {exception}",
                transport_text(&transport, "Local")
            )),
            None,
        )),
    }
}

/// The JDK's unprivileged `tcp_ping4`/`tcp_ping6`: `Ok(reachable)`, or the
/// `toString()` of the IOException `isReachable` would throw.
fn tcp_echo(address: SocketAddr, timeout: i32) -> Result<bool, String> {
    use nix::{
        errno::Errno,
        poll::{PollFd, PollFlags, PollTimeout, poll},
        sys::socket::{
            AddressFamily, SockFlag, SockProtocol, SockType, SockaddrStorage, connect, getsockopt, socket, sockopt,
        },
    };
    use std::{
        os::fd::{AsFd, AsRawFd},
        time::{Duration, Instant},
    };
    // JNU_ThrowByNameWithMessageAndLastError: "<strerror> (<detail>)".
    let exception = |kind: marathon_error::FailureKind, errno: Errno, detail: &str| {
        let text = std::io::Error::from_raw_os_error(errno as i32).to_string();
        let text = text.rsplit_once(" (os error ").map_or(text.as_str(), |(text, _)| text);
        format!("{kind}: {text} ({detail})")
    };
    let socket = socket(
        if address.is_ipv4() {
            AddressFamily::Inet
        } else {
            AddressFamily::Inet6
        },
        SockType::Stream,
        SockFlag::empty(),
        SockProtocol::Tcp,
    )
    .map_err(|errno| exception(marathon_error::FailureKind::Socket, errno, "Can't create socket"))?;
    nix::fcntl::fcntl(&socket, nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK))
        .map_err(|errno| exception(marathon_error::FailureKind::Socket, errno, "Can't create socket"))?;
    match connect(socket.as_raw_fd(), &SockaddrStorage::from(address)) {
        Ok(()) | Err(Errno::ECONNREFUSED) => return Ok(true),
        Err(Errno::EINPROGRESS) => {}
        Err(Errno::ENETUNREACH | Errno::EAFNOSUPPORT | Errno::EADDRNOTAVAIL) => return Ok(false),
        #[cfg(target_os = "linux")]
        Err(Errno::EINVAL | Errno::EHOSTUNREACH) => return Ok(false),
        Err(errno) => return Err(exception(marathon_error::FailureKind::Connect, errno, "connect failed")),
    }
    // NET_Wait(NET_WAIT_CONNECT): poll POLLOUT, retrying failed or early polls
    // until under a millisecond of the budget remains.
    let deadline = Instant::now() + Duration::from_millis(timeout as u64);
    let mut remaining = timeout;
    let ready = loop {
        let mut descriptors = [PollFd::new(socket.as_fd(), PollFlags::POLLOUT)];
        let polled = poll(
            &mut descriptors,
            PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX),
        );
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_millis(1) {
            break matches!(polled, Ok(n) if n > 0);
        }
        if matches!(polled, Ok(n) if n > 0) {
            break true;
        }
        remaining = left.as_millis() as i32;
    };
    if !ready {
        return Ok(false);
    }
    let error = getsockopt(&socket, sockopt::SocketError).unwrap_or_else(|errno| errno as i32);
    Ok(error == 0 || error == Errno::ECONNREFUSED as i32)
}
