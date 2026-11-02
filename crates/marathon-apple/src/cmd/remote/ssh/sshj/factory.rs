//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutorFactory.kt
//! and of `withRetrySync` from core/src/main/kotlin/com/malinskiy/marathon/execution/Retry.kt (its only caller).
//!
//! Plain parameters only: mapping `SshConfiguration` onto them is ConnectionFactory's (4.3).

use super::auth::SshAuthentication;
use super::client::{ClientConfig, RusshClient, SshClient};
use super::config::{PerformanceDefaultConfig, SshLogging};
use super::error::{ILLEGAL_STATE_EXCEPTION, SshFailure};
use super::executor::SshCommandExecutor;
use super::host::SshCommandHost;
use super::keep_alive::KeepAliveRunner;
use super::known_hosts::{HostKeyVerifier, PromiscuousVerifier};
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger},
};
use marathon_core::runtime::{VendorClock, VendorRuntime};
use std::{future::Future, sync::Arc};
use tokio::sync::oneshot;

pub struct SshjCommandExecutorFactory {
    pub connection_attempts: i32,
    pub keep_alive_interval: Duration,
    pub keep_alive_max_count: i32,
    /// `Channel.BUFFERED`.
    pub channel_capacity: usize,
    logging: SshLogging,
    logger: Logger,
    runtime: Arc<dyn VendorRuntime>,
    clock: Arc<dyn VendorClock>,
}

/// A client that connected and authenticated, before it becomes an executor.
pub struct Connected<C> {
    pub host: SshCommandHost,
    pub client: C,
}

impl SshjCommandExecutorFactory {
    /// russh runs on `runtime`; `clock` times sshj's 30 s replies and the keep-alive.
    pub fn new(logging: SshLogging, runtime: Arc<dyn VendorRuntime>, clock: Arc<dyn VendorClock>) -> Self {
        Self {
            runtime,
            clock,
            connection_attempts: 4,
            keep_alive_interval: Duration::of_seconds(15),
            keep_alive_max_count: 4,
            channel_capacity: 64,
            logger: logging.logger("com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorFactory"),
            logging,
        }
    }

    /// `connect(addr, port, authentication, hostKeyVerifier = PromiscuousVerifier(), debug)`. The whole exchange
    /// runs on the vendor runtime.
    pub async fn connect(
        &self,
        addr: &str,
        port: u16,
        authentication: SshAuthentication,
        host_key_verifier: Option<Arc<dyn HostKeyVerifier>>,
        debug: bool,
    ) -> Result<SshCommandExecutor, SshFailure> {
        let (runtime, clock) = (self.runtime.clone(), self.clock.clone());
        let verifier = host_key_verifier.unwrap_or_else(|| Arc::new(PromiscuousVerifier));
        let factory = Self {
            connection_attempts: self.connection_attempts,
            keep_alive_interval: self.keep_alive_interval,
            keep_alive_max_count: self.keep_alive_max_count,
            channel_capacity: self.channel_capacity,
            logging: self.logging.clone(),
            logger: self.logger.clone(),
            runtime: runtime.clone(),
            clock: clock.clone(),
        };
        let addr = addr.to_owned();
        let (tx, rx) = oneshot::channel();
        runtime.spawn(Box::pin(async move {
            let new_client = || factory.create_client(&addr, port, verifier.clone(), debug);
            let result = match factory.connect_with(new_client, &addr, port, authentication).await {
                Ok(connected) => {
                    let config = connected.client.config.loggers.config.clone();
                    connected.client.into_connection().map(|connection| {
                        SshCommandExecutor::new(connected.host, connection, factory.channel_capacity, config)
                    })
                }
                Err(error) => Err(error),
            };
            // A caller that stopped waiting (cancelled connect) leaves nobody to close the connection.
            if let Err(Ok(executor)) = tx.send(result) {
                let _ = executor.connection().close().await;
            }
            Ok(())
        }));
        rx.await
            .unwrap_or_else(|e| Err(SshFailure::new(ILLEGAL_STATE_EXCEPTION, e.to_string())))
    }

    /// The Kotlin tests' `internal connect(newClient, …)`. Every attempt uses a fresh client: sshj replaces the
    /// socket on each connect without closing the previous one. A client that failed to connect or authenticate
    /// is closed. Authentication runs once, outside the retry.
    pub async fn connect_with<C: SshClient>(
        &self,
        mut new_client: impl FnMut() -> C,
        addr: &str,
        port: u16,
        authentication: SshAuthentication,
    ) -> Result<Connected<C>, SshFailure> {
        let mut client = with_retry_sync(self.connection_attempts, &self.logging, &self.logger, || {
            let mut client = new_client();
            async move {
                let result = client.connect(addr, port).await;
                close_on_failure(&mut client, result).await.map(|()| client)
            }
        })
        .await?;
        let result = authentication.authenticate(&mut client).await;
        close_on_failure(&mut client, result).await?;
        Ok(Connected {
            host: SshCommandHost::new(addr, port, authentication),
            client,
        })
    }

    /// `createClient`: interval in whole seconds, the reply-expecting keep-alive, the verifier.
    pub fn create_client(
        &self,
        _addr: &str,
        _port: u16,
        host_key_verifier: Arc<dyn HostKeyVerifier>,
        debug: bool,
    ) -> RusshClient {
        let config = PerformanceDefaultConfig::new(debug, self.logging.clone());
        RusshClient::new(ClientConfig {
            keep_alive: KeepAliveRunner::new(self.keep_alive_interval, self.keep_alive_max_count),
            verifier: host_key_verifier,
            loggers: config.loggers(),
            runtime: self.runtime.clone(),
            clock: self.clock.clone(),
        })
    }
}

/// `closeOnFailure`: close the client and attach the close error as suppressed.
async fn close_on_failure<C: SshClient, T>(client: &mut C, result: Result<T, SshFailure>) -> Result<T, SshFailure> {
    match result {
        Ok(value) => Ok(value),
        Err(mut error) => {
            if let Err(close_error) = client.close().await {
                error.suppressed.push(close_error);
            }
            Err(error)
        }
    }
}

/// `withRetrySync(attempts, 0, logger)`: every failure is retried, logged at WARN, until the last attempt's is
/// rethrown. The only caller passes a delay of 0, which `Thread.sleep` makes a no-op, so there is no delay here.
pub async fn with_retry_sync<T, Fut: Future<Output = Result<T, SshFailure>>>(
    attempts: i32,
    logging: &SshLogging,
    logger: &Logger,
    mut f: impl FnMut() -> Fut,
) -> Result<T, SshFailure> {
    let mut attempt = 1;
    loop {
        match f().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt == attempts => return Err(error),
            Err(error) => logging.log(logger, Level::Warn, || "Retrying after failure".into(), Some(&error)),
        }
        attempt += 1;
    }
}
