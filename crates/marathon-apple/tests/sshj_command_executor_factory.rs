//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExecutorFactoryTest.kt
//! The Kotlin mock `SSHClient` becomes a scripted `SshClient` behind the factory's client seam.
mod support;
use marathon_apple::cmd::remote::ssh::sshj::{
    SshAuthentication, SshFailure,
    client::{SshClient, SshFuture},
    error::{DisconnectReason, TRANSPORT_EXCEPTION, USER_AUTH_EXCEPTION},
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use support::ssh;

type Calls = Arc<Mutex<Vec<String>>>;
struct Client {
    calls: Calls,
    connect: Option<SshFailure>,
    auth: Option<SshFailure>,
}
impl Client {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}
impl SshClient for Client {
    type Keys = ();
    fn connect<'a>(&'a mut self, addr: &'a str, port: u16) -> SshFuture<'a, ()> {
        self.record(format!("connect {addr} {port}"));
        let result = self.connect.clone().map_or(Ok(()), Err);
        Box::pin(async move { result })
    }
    fn auth_password<'a>(&'a mut self, username: &'a str, password: &'a str) -> SshFuture<'a, ()> {
        self.record(format!("authPassword {username} {password}"));
        let result = self.auth.clone().map_or(Ok(()), Err);
        Box::pin(async move { result })
    }
    fn load_keys<'a>(&'a mut self, _: &'a Path) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn auth_publickey<'a>(&'a mut self, _: &'a str, _: ()) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close(&mut self) -> SshFuture<'_, ()> {
        self.record("close".into());
        Box::pin(async { Ok(()) })
    }
}
fn authentication() -> SshAuthentication {
    SshAuthentication::Password {
        username: "marathon".into(),
        password: "test-password".into(),
    }
}
fn connected_client(calls: &Calls) -> Client {
    Client {
        calls: calls.clone(),
        connect: None,
        auth: None,
    }
}
fn failing_client(calls: &Calls) -> Client {
    Client {
        calls: calls.clone(),
        connect: Some(SshFailure::transport(DisconnectReason::Unknown, "Connection reset")),
        auth: None,
    }
}

#[tokio::test]
async fn client_is_closed_when_authentication_fails() {
    let calls = Calls::default();
    let mut client = Some(connected_client(&calls));
    client.as_mut().unwrap().auth = Some(SshFailure::new(
        USER_AUTH_EXCEPTION,
        "Exhausted available authentication methods",
    ));
    let (logging, _) = ssh::logging(false);
    let error = ssh::factory(&logging)
        .connect_with(|| client.take().unwrap(), "worker", 22, authentication())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, USER_AUTH_EXCEPTION);
    assert!(calls.lock().unwrap().contains(&"close".to_owned()));
}

#[tokio::test]
async fn every_failed_connection_attempt_closes_its_own_client() {
    let clients: Vec<Calls> = (0..2).map(|_| Calls::default()).collect();
    let mut next = clients.iter().map(failing_client).collect::<Vec<_>>().into_iter();
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.connection_attempts = 2;
    let error = factory
        .connect_with(|| next.next().unwrap(), "worker", 22, authentication())
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, TRANSPORT_EXCEPTION);
    for calls in &clients {
        let calls = calls.lock().unwrap();
        assert!(calls.contains(&"connect worker 22".to_owned()));
        assert!(calls.contains(&"close".to_owned()));
        assert!(!calls.iter().any(|call| call.starts_with("authPassword")));
    }
}

#[tokio::test]
async fn client_that_connects_after_failed_attempts_stays_open() {
    let (failed, connected) = (Calls::default(), Calls::default());
    let mut next = vec![failing_client(&failed), connected_client(&connected)].into_iter();
    let (logging, _) = ssh::logging(false);
    let mut factory = ssh::factory(&logging);
    factory.connection_attempts = 2;
    let executor = factory
        .connect_with(|| next.next().unwrap(), "worker", 22, authentication())
        .await
        .ok()
        .unwrap();
    assert_eq!(executor.host.id(), "marathon@worker:22");
    assert!(failed.lock().unwrap().contains(&"close".to_owned()));
    assert!(
        connected
            .lock()
            .unwrap()
            .contains(&"authPassword marathon test-password".to_owned())
    );
    assert!(!connected.lock().unwrap().contains(&"close".to_owned()));
}

/// Ours: Retry.kt's WARN carries the throwable; the last failure is rethrown unchanged.
#[tokio::test]
async fn retries_log_each_failure_and_rethrow_the_last() {
    let calls = Calls::default();
    let mut attempt = 0;
    let (logging, events) = ssh::logging(true);
    let error = ssh::factory(&logging)
        .connect_with(
            || {
                attempt += 1;
                Client {
                    calls: calls.clone(),
                    connect: Some(SshFailure::transport(
                        DisconnectReason::Unknown,
                        format!("attempt {attempt}"),
                    )),
                    auth: None,
                }
            },
            "worker",
            22,
            authentication(),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.message().as_deref(), Some("attempt 4"));
    let events = events.lock().unwrap();
    let warnings: Vec<_> = events
        .iter()
        .map(|(logger, level, message, throwable)| {
            (
                logger.as_str(),
                *level,
                message.as_str(),
                throwable.as_ref().and_then(|t| t.message.to_owned()),
            )
        })
        .collect();
    let logger = "com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutorFactory";
    assert_eq!(
        warnings,
        (1..=3)
            .map(|n| (
                logger,
                jcompat::logging::Level::Warn,
                "Retrying after failure",
                Some(format!("attempt {n}"))
            ))
            .collect::<Vec<_>>()
    );
}
