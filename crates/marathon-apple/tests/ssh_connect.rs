//! Connect, authentication, host keys and the failure taxonomy (T12, T13, T17, T19, T23-T26) against the fake
//! worker, compared with the Kotlin outcomes in `fixtures/fake-worker/evidence` and `fixtures/oracle/ssh`.
mod support;
use fake_worker::script::{Disconnect, Faults, Script};
use marathon_apple::{
    cmd::{
        CommandExecutor, CommandRequest,
        remote::ssh::sshj::{
            SshAuthentication, SshCommandExecutor, SshFailure,
            config::SshLogging,
            error::*,
            known_hosts::{HostKeyVerifier, OpenSshKnownHosts, md5_fingerprint},
        },
    },
    device::{
        connection_factory::{
            AddressProbe, ConnectionError, ConnectionFactory, ConnectionFuture, Connector,
            DEFAULT_MAX_CONCURRENT_CONNECTS, Executor, PreparedConnection,
        },
        connection_failures::is_transient_connection_failure,
    },
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use marathon_config::{
    devices::Transport,
    vendor::{RsyncConfiguration, SshConfiguration},
};
use marathon_core::error::VendorError;
use marathon_logging::Level;
use russh::keys::{Algorithm, PrivateKey};
use std::{path::Path, sync::Arc};
use support::{context, duration, ssh};

/// `[{class simple name, message, reason}]` down the cause chain, as the Kotlin evidence records it.
fn chain(failure: &SshFailure) -> Vec<(String, Option<String>, Option<&'static str>)> {
    let mut out = Vec::new();
    let mut current = Some(failure);
    while let Some(f) = current {
        let simple = f.kind.identifier().into_owned();
        out.push((simple, f.message(), f.is_ssh().then(|| f.reason.name())));
        current = f.cause.as_deref();
    }
    out
}
fn evidence(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-worker/evidence/round2-macos-faults/normalized")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(text.lines().next().unwrap()).unwrap()
}
fn evidence_chain(value: &serde_json::Value) -> Vec<(String, Option<String>, Option<&'static str>)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let reason = e.get("reason").map(|r| {
                (0..16)
                    .map(DisconnectReason::from_int)
                    .find(|d| d.name() == r.as_str().unwrap())
                    .unwrap()
                    .name()
            });
            (
                marathon_test_support::recorded_errors::ssh_kind(e["class"].as_str().unwrap())
                    .identifier()
                    .into(),
                e["message"].as_str().map(Into::into),
                reason,
            )
        })
        .collect()
}
fn faults(faults: Faults) -> Script {
    Script {
        faults,
        ..ssh::suite_script()
    }
}
async fn connections(fake: &ssh::Fake) -> usize {
    fake.events("connection").await.len()
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_before_kex_is_retried_four_times_with_the_wire_reason() {
    for reason in [12, 2, 11] {
        let fake = ssh::fake(
            faults(Faults {
                before_kex: Some(Disconnect {
                    reason,
                    message: format!("fixture disconnect {reason}"),
                }),
                ..Default::default()
            }),
            4,
        )
        .await;
        let (logging, events) = ssh::logging(false);
        let error = ssh::connect_with(&fake, &ssh::factory(&logging), false)
            .await
            .err()
            .unwrap();
        let expected = evidence(&format!("before_kex-{reason}.stdout"));
        assert_eq!(chain(&error), evidence_chain(&expected["value"]), "reason {reason}");
        assert_eq!(connections(&fake).await, 4);
        // Default-visible sshj lines per attempt, as in the Kotlin stderr evidence.
        let lines = ssh::lines(&events);
        assert_eq!(lines.len(), 8, "{lines:?}");
        assert_eq!(
            lines[0],
            (
                "marathon_apple::cmd::remote::ssh::sshj::wire".into(),
                Level::ERROR,
                format!("Dying because - fixture disconnect {reason}")
            )
        );
        assert_eq!(
            lines[1],
            (
                "marathon_apple::cmd::remote::ssh::sshj::wire".into(),
                Level::ERROR,
                format!("<<kex done>> woke to: {error}")
            )
        );
    }
}

async fn wait_disconnected(executor: &SshCommandExecutor) {
    for _ in 0..6000 {
        if !executor.connected() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the connection did not end");
}
fn drops_after_auth() -> Script {
    faults(Faults {
        after_auth: Some(Disconnect {
            reason: 11,
            message: "fixture disconnect 11".into(),
        }),
        ..Default::default()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_after_auth_makes_safe_execute_return_null_d36() {
    let fake = ssh::fake(drops_after_auth(), 4).await;
    let (logging, events) = ssh::logging(false);
    let executor = ssh::connect_with(&fake, &ssh::factory(&logging), false).await.unwrap();
    wait_disconnected(&executor).await;
    let request = || CommandRequest::new(["echo", "hello"], duration(5000));
    assert_eq!(executor.safe_execute(request(), context()).await, Ok(None));
    let safe_logged = (
        "marathon_apple::cmd::remote::ssh::sshj::client".to_owned(),
        Level::ERROR,
        "Unable to start a remote shell session [echo, hello]".to_owned(),
    );
    assert!(ssh::lines(&events).contains(&safe_logged), "{:?}", ssh::lines(&events));
    // The failure keeps Kotlin's text, says why, and is a class every caller already treats as a lost connection.
    let error = executor.connection().start_session().await.err().unwrap();
    assert_eq!(
        chain(&error),
        [
            ("ssh.connection".into(), Some("Not connected".into()), Some("UNKNOWN")),
            (
                "ssh.transport".into(),
                Some("fixture disconnect 11".into()),
                Some("BY_APPLICATION")
            ),
        ]
    );
    let Err(VendorError::Other(failure)) = executor.execute(request(), context()).await else {
        panic!("execute must fail");
    };
    assert_eq!(failure, error.to_failure());
    // What recovery reads: `connected()`, which is false by the same check that refused the command.
    assert!(!executor.connected());
    // The JVM recording stays as the witness of what A17 was.
    let kotlin = evidence("after_auth-11.stdout");
    assert_eq!(
        marathon_test_support::recorded_errors::ssh_kind(kotlin["value"][0]["class"].as_str().unwrap()),
        marathon_error::FailureKind::IllegalState
    );
    assert_eq!(kotlin["value"][0]["message"], "Not connected");
    assert_eq!(connections(&fake).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_connection_the_server_already_dropped_keeps_it_a_lost_connection_d36() {
    // Disposal closes the executor of a lost worker while a task on that device is still running. That task did
    // nothing wrong: its command must get the null of a lost connection, not the state error of a closed client.
    let fake = ssh::fake(drops_after_auth(), 4).await;
    let executor = ssh::connect(&fake).await;
    wait_disconnected(&executor).await;
    executor.close().await.unwrap();
    assert_eq!(
        executor
            .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
            .await,
        Ok(None)
    );
    let error = executor.connection().start_session().await.err().unwrap();
    assert_eq!(
        chain(&error)[1],
        (
            "ssh.transport".into(),
            Some("fixture disconnect 11".into()),
            Some("BY_APPLICATION")
        ),
        "the worker's reason stays, not the local close's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_always_drops_after_auth_costs_one_connection_per_acquire_d36() {
    // No path may turn a null into a reconnect storm: the executor never reconnects by itself, however often a
    // command is refused, and the factory replaces a dead shared connection once per acquire.
    let fake = ssh::fake(drops_after_auth(), 4).await;
    let (logging, _) = ssh::logging(false);
    let factory = ConnectionFactory::with_seams(
        false,
        SshConfiguration {
            authentication: Some(marathon_config::vendor::SshAuthentication::Password {
                username: "fixture".into(),
                password: "fake-password".into(),
            }),
            known_hosts_path: None,
            keep_alive_interval: duration(0),
            keep_alive_max_count: 4,
            debug: false,
            share_worker_connection: true,
        },
        RsyncConfiguration {
            remote_path: "/usr/bin/rsync".into(),
        },
        duration(5000),
        DEFAULT_MAX_CONCURRENT_CONNECTS,
        Arc::new(support::Runtime),
        Arc::new(support::Clock::default()),
        logging.clone(),
        Arc::new(FakeConnector(fake.port(), logging)),
        Arc::new(NoProbe),
    )
    .unwrap();
    let worker = Transport::Ssh {
        addr: "127.0.0.1".into(),
        port: i32::from(fake.port()),
        authentication: None,
        check_reachability: false,
    };
    let mut acquired = Vec::new();
    for round in 1..=5 {
        let (executor, _) = factory.create_or_throw(worker.clone(), context()).await.unwrap();
        for _ in 0..3 {
            for _ in 0..6000 {
                if !executor.connected() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(
                executor
                    .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
                    .await,
                Ok(None)
            );
        }
        acquired.push(executor);
        assert_eq!(connections(&fake).await, round);
    }
    for executor in acquired {
        factory.dispose(executor).await.unwrap();
    }
    assert_eq!(connections(&fake).await, 5);
}
struct FakeConnector(u16, SshLogging);
impl Connector for FakeConnector {
    fn prepare(&self, _: Transport) -> ConnectionFuture<'_, PreparedConnection> {
        let (port, logging) = (self.0, self.1.clone());
        Box::pin(async move {
            Ok(Box::new(move || {
                Box::pin(async move {
                    let auth = SshAuthentication::Password {
                        username: "fixture".into(),
                        password: "fake-password".into(),
                    };
                    let executor = ssh::factory(&logging)
                        .connect("127.0.0.1", port, auth, None, false)
                        .await?;
                    Ok(Arc::new(executor) as Executor)
                }) as ConnectionFuture<'static, Executor>
            }) as PreparedConnection)
        })
    }
}
struct NoProbe;
impl AddressProbe for NoProbe {
    fn check(&self, _: Transport, _: jcompat::jtime::Duration) -> ConnectionFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_during_exec_fails_execute_with_the_wire_reason() {
    for reason in [12, 2, 11] {
        let fake = ssh::fake(
            faults(Faults {
                during_exec: Some(Disconnect {
                    reason,
                    message: format!("fixture disconnect {reason}"),
                }),
                ..Default::default()
            }),
            4,
        )
        .await;
        let (logging, events) = ssh::logging(false);
        let executor = ssh::connect_with(&fake, &ssh::factory(&logging), false).await.unwrap();
        let error = match executor
            .execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
            .await
        {
            Err(VendorError::Other(failure)) => failure,
            _ => panic!("execute must fail"),
        };
        let expected = evidence(&format!("during_exec-{reason}.stdout"));
        let simple: Vec<_> = std::iter::successors(Some(&error), |f| f.cause.as_deref())
            .map(|f| {
                (
                    f.kind.identifier().into_owned(),
                    f.message.as_ref().map(|m| m.to_owned()),
                )
            })
            .collect();
        let kotlin: Vec<_> = evidence_chain(&expected["value"])
            .into_iter()
            .map(|(c, m, _)| (c, m))
            .collect();
        assert_eq!(simple, kotlin, "reason {reason}");
        assert!(!executor.connected());
        let lines = ssh::lines(&events);
        assert_eq!(
            lines[0],
            (
                "marathon_apple::cmd::remote::ssh::sshj::wire".into(),
                Level::ERROR,
                format!("Dying because - fixture disconnect {reason}")
            )
        );
        assert_eq!(
            lines[1],
            (
                "marathon_apple::cmd::remote::ssh::sshj::wire".into(),
                Level::ERROR,
                format!("<<chan#0 / chanreq for exec>> woke to: SSH connection failure: fixture disconnect {reason}")
            )
        );
        // D36: the next command finds the transport gone, and safeExecute turns that into null too.
        assert_eq!(
            executor
                .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
                .await,
            Ok(None)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_common_kex_is_transport_unknown_after_four_attempts_and_not_transient_d35() {
    let fake = ssh::fake(
        faults(Faults {
            no_common_kex: true,
            ..Default::default()
        }),
        4,
    )
    .await;
    let (logging, _) = ssh::logging(false);
    let error = ssh::connect_with(&fake, &ssh::factory(&logging), false)
        .await
        .err()
        .unwrap();
    // T26: what the user sees stays sshj's. Algorithm lists are russh's, the shape sshj's. D35 changes only what the
    // classifier makes of it (A16), and every attempt still runs, as for an unverifiable host key.
    assert_eq!(error.kind, TRANSPORT_EXCEPTION);
    assert_eq!(error.reason, DisconnectReason::Unknown);
    assert!(error.is_no_common_algorithm());
    assert!(
        error
            .to_string()
            .starts_with(&format!("{TRANSPORT_EXCEPTION}: Unable to reach a settlement of "))
    );
    assert!(!is_transient_connection_failure(&ConnectionError::Device(
        DeviceFailureException::new(DeviceFailureReason::UnreachableHost, None, Some(error.clone()))
    )));
    let message = error.message().unwrap();
    assert!(
        message.starts_with("Unable to reach a settlement of KeyExchangeAlgorithms: ["),
        "{message}"
    );
    assert!(
        evidence("kex-failure.stdout")["value"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("Unable to reach a settlement of KeyExchangeAlgorithms: [")
    );
    assert_eq!(connections(&fake).await, 4);
}

fn auth_events(events: &[serde_json::Value], connection: u64) -> Vec<(String, bool)> {
    events
        .iter()
        .filter(|e| e["connection"] == connection && (e["event"] == "auth" || e["event"] == "auth_challenge"))
        .map(|e| match e["event"].as_str().unwrap() {
            "auth" => (
                e["detail"]["method"].as_str().unwrap().to_owned(),
                e["detail"]["accepted"].as_bool().unwrap(),
            ),
            _ => ("challenge".to_owned(), true),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn password_authentication_uses_sshj_method_sequence() {
    // Correct password: `password` only (Kotlin evidence round2-macos-standard connection 5).
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    assert_eq!(
        auth_events(&fake.transcript().await, 1),
        [("password".to_owned(), true)]
    );
    executor.close().await.unwrap();

    // Wrong password: `password`, then keyboard-interactive answering "Password: ", then one exhausted failure
    // without retry (T12; Kotlin evidence connection 12).
    let (logging, _) = ssh::logging(false);
    let error = ssh::factory(&logging)
        .connect(
            "127.0.0.1",
            fake.port(),
            SshAuthentication::Password {
                username: "fixture".into(),
                password: "wrong".into(),
            },
            None,
            false,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(
        chain(&error),
        [(
            "ssh.authentication".into(),
            Some("Exhausted available authentication methods".into()),
            Some("UNKNOWN")
        )]
    );
    assert_eq!(connections(&fake).await, 2);
    assert_eq!(
        auth_events(&fake.transcript().await, 2),
        [
            ("password".into(), false),
            ("challenge".into(), true),
            ("keyboard-interactive".into(), false)
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_interactive_answers_the_password_prompt() {
    // A server without the `password` method (Kotlin evidence run "keyboard", connection 1).
    let fake = ssh::fake_with(ssh::suite_script(), 4, |config| config.auth.password_auth = false).await;
    let executor = ssh::connect(&fake).await;
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
    assert_eq!(
        auth_events(&fake.transcript().await, 1),
        [
            ("password".into(), false),
            ("challenge".into(), true),
            ("keyboard-interactive".into(), true)
        ]
    );
    for (prompt, answered) in [
        ("Password: ", true),
        ("Password:", true),
        ("password:", true),
        ("Enter password: ", true),
        ("Password for fixture@worker: ", true),
        ("Password", false),
        ("Verification code: ", false),
        ("Password:  ", false),
    ] {
        assert_eq!(
            marathon_apple::cmd::remote::ssh::sshj::client::password_prompt(prompt),
            answered,
            "{prompt:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn public_key_authentication_and_key_file_failures() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let key = fake.dir.path().join("client");
    std::fs::write(
        &key,
        ssh::client_key_pair()
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let (logging, _) = ssh::logging(false);
    let factory = ssh::factory(&logging);
    let auth = |key: &Path| SshAuthentication::PublicKey {
        username: "fixture".into(),
        key: key.into(),
    };
    let executor = factory
        .connect("127.0.0.1", fake.port(), auth(&key), None, false)
        .await
        .unwrap();
    let result = ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap();
    assert_eq!(result.stdout, ["hello"]);
    executor.close().await.unwrap();
    assert_eq!(
        auth_events(&fake.transcript().await, 1),
        [("publickey_offer".into(), true), ("publickey".into(), true)]
    );

    // T13: a missing key file is FileNotFoundException, not wrapped and not retried.
    let missing = fake.dir.path().join("absent");
    let error = factory
        .connect("127.0.0.1", fake.port(), auth(&missing), None, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, FILE_NOT_FOUND_EXCEPTION);
    assert_eq!(
        error.message(),
        Some(format!("{} (No such file or directory)", missing.display()))
    );

    // An encrypted key without passphrase fails inside UserAuthException.
    let mut encrypted = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    encrypted = encrypted.encrypt(&mut rand::rng(), "secret").unwrap();
    let path = fake.dir.path().join("encrypted");
    std::fs::write(
        &path,
        encrypted
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let error = factory
        .connect("127.0.0.1", fake.port(), auth(&path), None, false)
        .await
        .err()
        .unwrap();
    let classes: Vec<_> = chain(&error).into_iter().map(|(class, ..)| class).collect();
    assert_eq!(
        classes,
        [
            "ssh.authentication",
            "ssh.authentication",
            "ssh.key-decryption-failed",
            "ssh.encryption"
        ]
    );
    assert!(!error.is_instance(TRANSPORT_EXCEPTION));
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_connection_is_retried_and_rethrown() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (logging, events) = ssh::logging(true);
    let error = ssh::factory(&logging)
        .connect("127.0.0.1", port, ssh::password(), None, false)
        .await
        .err()
        .unwrap();
    // T12: java.net.ConnectException, a SocketException, after four attempts.
    assert_eq!(
        chain(&error),
        [("connect".into(), Some("Connection refused".into()), None)]
    );
    assert!(error.is_instance(SOCKET_EXCEPTION) && error.is_io());
    assert_eq!(
        ssh::lines(&events)
            .iter()
            .filter(|(_, level, m)| *level == Level::WARN && m == "Retrying after failure")
            .count(),
        3
    );
    let error = ssh::factory(&logging)
        .connect("nonexistent.invalid", 22, ssh::password(), None, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, UNKNOWN_HOST_EXCEPTION);
    assert!(error.message().unwrap().starts_with("nonexistent.invalid: "), "{error}");
}

fn oracle_connect(case: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/ssh/connect.jsonl");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["case"] == case)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_host_key_is_rejected_four_times_like_sshj() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let port = fake.port().to_string();
    let fingerprint = md5_fingerprint(fake.host_key.public_key());
    for (case, file) in [
        ("connect_empty", "empty_known_hosts"),
        ("connect_wrong", "wrong_known_hosts"),
    ] {
        let path = fake.dir.path().join(file);
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let text = if case == "connect_wrong" {
            format!("[127.0.0.1]:{port} {}\n", other.public_key().to_openssh().unwrap())
        } else {
            String::new()
        };
        std::fs::write(&path, text).unwrap();
        let (logging, events) = ssh::logging(false);
        let verifier: Arc<dyn HostKeyVerifier> = Arc::new(OpenSshKnownHosts::load(&path, logging.clone()).unwrap());
        let error = ssh::factory(&logging)
            .connect("127.0.0.1", fake.port(), ssh::password(), Some(verifier), false)
            .await
            .err()
            .unwrap();
        let oracle = oracle_connect(case);
        let localise = |text: &str| {
            text.replace(oracle["fingerprint"].as_str().unwrap(), &fingerprint)
                .replace("<port>", &port)
                .replace("<dir>", &fake.dir.path().display().to_string())
        };
        assert_eq!(
            error.to_string(),
            marathon_test_support::recorded_errors::rendered(&localise(
                oracle["error"][0]["toString"].as_str().unwrap()
            ))
            .unwrap()
        );
        assert_eq!(error.reason, DisconnectReason::HostKeyNotVerifiable);
        // Every default-visible line, in order: the WARN lines are hidden at the default ERROR level except
        // the plain-slf4j "has changed" one, which ROOT decides.
        let expected: Vec<_> = oracle["log"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|line| line["level"] == "ERROR")
            .map(|line| {
                (
                    marathon_test_support::recorded_logs::target(line["logger"].as_str().unwrap()).to_owned(),
                    {
                        let message = localise(line["message"].as_str().unwrap());
                        if line["logger"] == "Promise" {
                            let rest = message
                                .strip_prefix("<<kex done>> woke to: ")
                                .expect("recorded Promise failure");
                            format!(
                                "<<kex done>> woke to: {}",
                                marathon_test_support::recorded_errors::rendered(rest).unwrap()
                            )
                        } else {
                            message
                        }
                    },
                )
            })
            .collect();
        let actual: Vec<_> = ssh::lines(&events)
            .into_iter()
            .filter(|(_, level, _)| *level == Level::ERROR)
            .map(|(logger, _, message)| (logger, message))
            .collect();
        assert_eq!(actual, expected, "{case}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn known_host_key_is_accepted_and_orders_the_proposal() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let path = fake.dir.path().join("known_hosts");
    std::fs::write(
        &path,
        format!(
            "[127.0.0.1]:{} {}\n",
            fake.port(),
            fake.host_key.public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let (logging, _) = ssh::logging(false);
    let verifier: Arc<dyn HostKeyVerifier> = Arc::new(OpenSshKnownHosts::load(&path, logging.clone()).unwrap());
    let factory = ssh::factory(&logging);
    let executor = factory
        .connect("127.0.0.1", fake.port(), ssh::password(), Some(verifier), false)
        .await
        .unwrap();
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );

    // KeyExchanger puts the known types first and keeps the configured order otherwise.
    // An RSA public key from the oracle table: generating one is slow in debug builds.
    let table = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/ssh/known_hosts.jsonl"),
    )
    .unwrap();
    let row: serde_json::Value = table
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .find(|row: &serde_json::Value| row["case"] == "other_algorithm_only")
        .unwrap();
    let rsa = row["known_hosts"].as_str().unwrap().split_once(' ').unwrap().1;
    let text = format!("worker {rsa}\n");
    let known: Arc<dyn HostKeyVerifier> = Arc::new(OpenSshKnownHosts::parse(&path, &text, logging.clone()).unwrap());
    assert_eq!(known.find_existing_algorithms("worker", 22), ["ssh-rsa"]);
    let client = factory.create_client("worker", 22, known, false);
    let keys = client.russh_config("worker", 22).preferred.key.to_vec();
    assert_eq!(keys[0], Algorithm::Rsa { hash: None });
    let mut rest = russh::Preferred::DEFAULT.key.to_vec();
    rest.retain(|a| *a != Algorithm::Rsa { hash: None });
    assert_eq!(keys[1..], rest[..]);
}
