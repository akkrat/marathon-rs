//! Connect, authentication, host keys and the failure taxonomy (T12, T13, T17, T19, T23-T26) against the fake
//! worker, compared with the Kotlin outcomes in `fixtures/fake-worker/evidence` and `fixtures/oracle/ssh`.
mod support;
use fake_worker::script::{Disconnect, Faults, Script};
use jcompat::logging::Level;
use marathon_apple::cmd::{
    CommandExecutor, CommandRequest,
    remote::ssh::sshj::{
        SshAuthentication, SshFailure,
        error::*,
        known_hosts::{HostKeyVerifier, OpenSshKnownHosts, md5_fingerprint},
    },
};
use marathon_core::error::VendorError;
use russh::keys::{Algorithm, PrivateKey};
use std::{path::Path, sync::Arc};
use support::{context, duration, ssh};

/// `[{class simple name, message, reason}]` down the cause chain, as the Kotlin evidence records it.
fn chain(failure: &SshFailure) -> Vec<(String, Option<String>, Option<&'static str>)> {
    let mut out = Vec::new();
    let mut current = Some(failure);
    while let Some(f) = current {
        let simple = f.class.rsplit(['.', '$']).next().unwrap().to_owned();
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
                e["class"].as_str().unwrap().into(),
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
                "TransportImpl".into(),
                Level::Error,
                format!("Dying because - fixture disconnect {reason}")
            )
        );
        assert_eq!(
            lines[1],
            (
                "Promise".into(),
                Level::Error,
                format!("<<kex done>> woke to: {}", error.java_to_string())
            )
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_after_auth_makes_execute_throw_not_connected() {
    let fake = ssh::fake(
        faults(Faults {
            after_auth: Some(Disconnect {
                reason: 11,
                message: "fixture disconnect 11".into(),
            }),
            ..Default::default()
        }),
        4,
    )
    .await;
    let executor = ssh::connect(&fake).await;
    for _ in 0..500 {
        if !executor.connected() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!executor.connected());
    // A17: not a ConnectionException or TransportException, so safeExecute propagates it.
    let error = executor
        .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
        .await
        .err()
        .unwrap();
    let expected = evidence("after_auth-11.stdout");
    assert_eq!(
        error,
        VendorError::Other(SshFailure::not_connected().to_failure()),
        "{expected}"
    );
    assert_eq!(expected["value"][0]["message"], "Not connected");
    assert_eq!(connections(&fake).await, 1);
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
                    f.class.to_string_lossy().rsplit('.').next().unwrap().to_owned(),
                    f.message.as_ref().map(|m| m.to_string_lossy()),
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
                "TransportImpl".into(),
                Level::Error,
                format!("Dying because - fixture disconnect {reason}")
            )
        );
        assert_eq!(
            lines[1],
            (
                "Promise".into(),
                Level::Error,
                format!(
                    "<<chan#0 / chanreq for exec>> woke to: net.schmizz.sshj.connection.ConnectionException: fixture disconnect {reason}"
                )
            )
        );
        // ConnectionException: safeExecute turns it into null with the 4.1 log line.
        assert_eq!(
            executor
                .safe_execute(CommandRequest::new(["echo", "hello"], duration(5000)), context())
                .await,
            Err(VendorError::Other(SshFailure::not_connected().to_failure()))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_common_kex_is_transport_unknown_after_four_attempts() {
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
    // T26 / A16: UNKNOWN, which 4.3's classifier calls transient. Algorithm lists are russh's, the shape sshj's.
    assert_eq!(error.class, TRANSPORT_EXCEPTION);
    assert_eq!(error.reason, DisconnectReason::Unknown);
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
            "UserAuthException".into(),
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
    assert_eq!(error.class, FILE_NOT_FOUND_EXCEPTION);
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
            "UserAuthException",
            "UserAuthException",
            "KeyDecryptionFailedException",
            "EncryptionException"
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
        [("ConnectException".into(), Some("Connection refused".into()), None)]
    );
    assert!(error.is_instance(SOCKET_EXCEPTION) && error.is_io());
    assert_eq!(
        ssh::lines(&events)
            .iter()
            .filter(|(_, level, m)| *level == Level::Warn && m == "Retrying after failure")
            .count(),
        3
    );
    let error = ssh::factory(&logging)
        .connect("nonexistent.invalid", 22, ssh::password(), None, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error.class, UNKNOWN_HOST_EXCEPTION);
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
            error.java_to_string(),
            localise(oracle["error"][0]["toString"].as_str().unwrap())
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
                    line["logger"].as_str().unwrap().to_owned(),
                    localise(line["message"].as_str().unwrap()),
                )
            })
            .collect();
        let actual: Vec<_> = ssh::lines(&events)
            .into_iter()
            .filter(|(_, level, _)| *level == Level::Error)
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
