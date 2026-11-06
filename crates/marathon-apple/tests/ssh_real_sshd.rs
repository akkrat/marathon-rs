//! Opt-in runs against a real OpenSSH server (orchestrator decision 5 and the real-host measurements): the generic
//! executor suite, and T04, T07, T16, T30 and T32 as Linux and macOS OpenSSH profiles. Ignored by default; set
//! `MARATHON_SSH_REAL=<host>:<port>:<user>:<private key path>` (a throwaway sshd, see docs/wp/4.2-findings.md) and
//! run `cargo test -p marathon-apple --test ssh_real_sshd -- --ignored --test-threads 1`. The D35 case also needs
//! `MARATHON_SSH_REAL_NO_KEX`, see its comment.
mod support;
use marathon_apple::{
    cmd::{
        CommandExecutor, CommandRequest,
        remote::ssh::sshj::{DisconnectReason, SshAuthentication, SshCommandExecutor},
    },
    device::{connection_factory::ConnectionError, connection_failures::is_transient_connection_failure},
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use std::time::{Duration, Instant};
use support::{context, duration, executor_suite as suite, ssh};

async fn connect() -> SshCommandExecutor {
    let target = std::env::var("MARATHON_SSH_REAL").expect("MARATHON_SSH_REAL=<host>:<port>:<user>:<key>");
    let mut parts = target.splitn(4, ':');
    let (host, port, user, key) = (
        parts.next().unwrap().to_owned(),
        parts.next().unwrap().parse().unwrap(),
        parts.next().unwrap().to_owned(),
        parts.next().unwrap().to_owned(),
    );
    let (logging, _) = ssh::logging(false);
    let auth = SshAuthentication::PublicKey {
        username: user,
        key: key.into(),
    };
    ssh::factory(&logging)
        .connect(&host, port, auth, None, true)
        .await
        .unwrap()
}

macro_rules! suite_case {
    ($name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "needs MARATHON_SSH_REAL"]
        async fn $name() {
            suite::$name(connect().await).await;
        }
    };
}
suite_case!(test_stdout);
suite_case!(test_stderr);
suite_case!(test_exit_code);
suite_case!(test_buffering);
suite_case!(test_consume_output_drains_streams_concurrently);
suite_case!(test_consume_output_failure_does_not_block_session_close);
suite_case!(test_timeout);
suite_case!(test_terminate);
suite_case!(test_terminate_should_not_close_executor);
suite_case!(test_interrupt_using_kill_should_not_close_executor);

fn request(command: &[&str], timeout: i64) -> CommandRequest {
    CommandRequest::new(command.iter().copied(), duration(timeout))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t04_env_and_workdir_are_ignored() {
    let executor = connect().await;
    let mut request = request(&["sh", "-c", "echo F=${F-unset}; pwd"], 10_000);
    request.env = [("F".to_owned(), "1".to_owned())].into();
    request.workdir = Some("/tmp".into());
    let session = executor.execute(request, context()).await.unwrap();
    let result = session.await_result(&context()).await.unwrap();
    session.close().await.unwrap();
    // A2: no env request and no cd, so the login directory (the home) is printed.
    assert_eq!((result.exit_code, result.stdout[0].as_str()), (Some(0), "F=unset"));
    assert_eq!(result.stdout.len(), 2);
    assert_ne!(result.stdout[1], "/tmp");
    println!("T04.result: exit=0 stdout=[F=unset, <home>] stderr={:?}", result.stderr);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t07_background_and_ignored_term() {
    let executor = connect().await;
    let os = ssh::run(&executor, &["uname", "-s"], 10_000).await.unwrap().stdout;
    let background = ssh::run(&executor, &["sleep", "100000", "&"], 600_000).await.unwrap();
    // Profiles measured with the Kotlin harness: GNU sleep with C locale quotes (Linux), BSD sleep (macOS).
    let expected: &[&str] = match os[0].as_str() {
        "Darwin" => &[
            "sleep: invalid time interval: &",
            "usage: sleep number[unit] [...]",
            "Unit can be 's' (seconds, the default), m (minutes), h (hours), or d (days).",
        ],
        _ => &[
            "sleep: invalid time interval '&'",
            "Try 'sleep --help' for more information.",
        ],
    };
    assert_eq!(background.exit_code, Some(1));
    assert_eq!(background.stderr, expected);
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );

    let ignoring = ["sh", "-c", "trap \"\" TERM; sleep 5; echo survived"];
    let session = executor.execute(request(&ignoring, 30_000), context()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let started = Instant::now();
    session.terminate().unwrap();
    let result = session.await_result(&context()).await.unwrap();
    let ignored_ms = started.elapsed().as_millis();
    assert_eq!(
        (result.exit_code, result.stdout.as_slice()),
        (Some(0), ["survived".to_owned()].as_slice())
    );
    session.close().await.unwrap();

    // close() = TERM if open, CLOSE, then the join; sshd answers CLOSE only when the process has ended (~4 s).
    let closing = executor.execute(request(&ignoring, 30_000), context()).await.unwrap();
    let ctx = context();
    let (awaited, (closed, close_ms)) = tokio::join!(closing.await_result(&ctx), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let started = Instant::now();
        (closing.close().await, started.elapsed().as_millis())
    });
    assert_eq!(closed, Ok(()));
    // Approved P2: Kotlin's await observed JobCancellationException here too.
    assert!(
        matches!(awaited, Err(marathon_core::error::VendorError::Cancelled(_))),
        "{awaited:?}"
    );
    assert!((3000..6000).contains(&close_ms), "{close_ms}");
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );
    println!("T07.info.ignored_term_ms: {ignored_ms}");
    println!("T07.info.close_ms: {close_ms}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t16_signals_other_than_term() {
    let executor = connect().await;
    for (signal, expected) in [
        ("KILL", None),
        ("SEGV", None),
        ("TERM", Some(143)),
        ("INT", None),
        ("HUP", None),
        ("USR2", None),
    ] {
        let result = ssh::run(&executor, &["sh", "-c", &format!("kill -{signal} $$")], 10_000)
            .await
            .unwrap();
        assert_eq!(result.exit_code, expected, "{signal}");
        tokio::time::sleep(Duration::from_millis(500)).await;
        // D7: no stale TERM after a non-TERM signal, so the shared connection always survives.
        let survives = ssh::run(&executor, &["echo", "hello"], 10_000).await.map(|r| r.stdout);
        assert_eq!(survives, Ok(vec!["hello".to_owned()]), "{signal}");
        println!("T16.{signal}: exit={:?} connection_survives=true", result.exit_code);
    }
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t14_tails_flood_close_and_idle_timeout() {
    let executor = connect().await;
    let partial = ssh::run(&executor, &["sh", "-c", "printf partial; sleep 3"], 1000)
        .await
        .unwrap();
    assert_eq!(
        (partial.exit_code, partial.stdout.as_slice()),
        (None, ["partial".to_owned()].as_slice())
    );
    let big = ssh::run(&executor, &["sh", "-c", "seq 1 300000; exit 3"], 60_000)
        .await
        .unwrap();
    assert_eq!(
        (big.exit_code, big.stdout.len(), big.stdout.last().map(String::as_str)),
        (Some(3), 300_000, Some("300000"))
    );

    // Kotlin on the same Linux sshd: close returned after 38 ms and the concurrent await was cancelled.
    let flood = executor
        .execute(request(&["seq", "1", "100000000"], 60_000), context())
        .await
        .unwrap();
    let ctx = context();
    let (awaited, (closed, close_ms)) = tokio::join!(flood.await_result(&ctx), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        (flood.close().await, started.elapsed().as_millis())
    });
    assert_eq!(closed, Ok(()));
    assert!(
        matches!(awaited, Err(marathon_core::error::VendorError::Cancelled(_))),
        "{awaited:?}"
    );
    assert!(close_ms < 5000, "{close_ms}");
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );

    let mut idle = request(&["sh", "-c", "echo a; sleep 3; echo b"], 10_000);
    idle.idle_timeout = duration(1000);
    let session = executor.execute(idle, context()).await.unwrap();
    let error = session.await_result(&context()).await.err().unwrap();
    let _ = session.close().await;
    // Kotlin: CancellationException(Channel closed) <- TimeoutException(idle timeout PT1S reached).
    let marathon_core::error::VendorError::Cancelled(marathon_core::error::Cancellation::Requested(cancel)) = error
    else {
        panic!("{error:?}")
    };
    let chain: Vec<_> = std::iter::successors(Some(&cancel), |f| f.cause.as_deref())
        .map(|f| {
            (
                f.class.to_string_lossy(),
                f.message.as_ref().map(|m| m.to_string_lossy()),
            )
        })
        .collect();
    assert_eq!(
        chain,
        [
            (
                "java.util.concurrent.CancellationException".to_owned(),
                Some("Channel closed".to_owned())
            ),
            (
                "java.util.concurrent.TimeoutException".to_owned(),
                Some("idle timeout PT1S reached".to_owned())
            ),
        ]
    );
    println!("T14.info.flood_close_ms: {close_ms}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t15t_timeout_on_a_process_that_ignores_term() {
    // Kotlin on the same Linux sshd: null after 8078 ms (sshd answers CLOSE when the process ends), then usable.
    let executor = connect().await;
    let ignoring = ["sh", "-c", "trap \"\" TERM; sleep 8; echo survived"];
    let session = executor.execute(request(&ignoring, 1000), context()).await.unwrap();
    let started = Instant::now();
    let result = session.await_result(&context()).await.unwrap();
    let await_ms = started.elapsed().as_millis();
    assert_eq!(result.exit_code, None);
    assert!((7000..10_000).contains(&await_ms), "{await_ms}");
    assert_eq!(session.close().await, Ok(()));
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );
    println!("T15t.info.await_ms: {await_ms}");
}

/// What the fake cannot show: on OpenSSH, KILL ends a process that ignores TERM and its whole group (`killpg`), and the
/// close then succeeds. Without KILL the close would fail after 30 s.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t30_kill_ends_a_process_that_ignores_term() {
    let executor = connect().await;
    // No `exec`: the child, which inherits the ignored TERM, must die too. The duration is this run's, because the probe
    // below matches command lines, and any other process that mentions a fixed number would match as well.
    let seconds = 100_000 + std::process::id();
    let body = format!("trap \"\" TERM; sleep {seconds}; echo survived");
    let ignoring = ["sh", "-c", body.as_str()];
    let session = executor.execute(request(&ignoring, 600_000), context()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let started = Instant::now();
    assert_eq!(session.close().await, Ok(()));
    let close_ms = started.elapsed().as_millis();
    assert!((19_000..29_000).contains(&close_ms), "{close_ms}");
    // sshd answers when the session's own process is reaped, so give the rest of its group a moment.
    let probe = format!("pgrep -fl \"[s]leep {seconds}\"");
    let left = ["sh", "-c", probe.as_str()];
    let mut found = Vec::new();
    for _ in 0..600 {
        found = ssh::run(&executor, &left, 10_000).await.unwrap().stdout;
        if found.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        found,
        Vec::<String>::new(),
        "a process of the session's group survived KILL"
    );
    println!("T30.info.close_ms: {close_ms}");
}

/// D32 against a real login shell and sshd: the variable reaches the process, while F and the workdir still do not (A2).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn t32_ns_unbuffered_io_reaches_the_process_and_nothing_else_does() {
    let executor = connect().await;
    let mut request = request(
        &["sh", "-c", "echo NS=${NSUnbufferedIO-unset} F=${F-unset}; pwd"],
        10_000,
    );
    request.env = [
        ("NSUnbufferedIO".to_owned(), "YES".to_owned()),
        ("F".to_owned(), "1".to_owned()),
    ]
    .into();
    request.workdir = Some("/tmp".into());
    let session = executor.execute(request, context()).await.unwrap();
    let result = session.await_result(&context()).await.unwrap();
    session.close().await.unwrap();
    assert_eq!(
        (result.exit_code, result.stdout[0].as_str()),
        (Some(0), "NS=YES F=unset")
    );
    assert_ne!(result.stdout[1], "/tmp");
    println!("T32.result: exit=0 stdout=[{}, <home>]", result.stdout[0]);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL_NO_KEX"]
async fn d35_no_common_kex_on_a_real_sshd_is_not_transient() {
    // MARATHON_SSH_REAL_NO_KEX=<host>:<port>: a throwaway sshd with `KexAlgorithms diffie-hellman-group1-sha1`,
    // which the client does not offer. The exchange fails before authentication.
    let target = std::env::var("MARATHON_SSH_REAL_NO_KEX").expect("MARATHON_SSH_REAL_NO_KEX=<host>:<port>");
    let (host, port) = target.split_once(':').unwrap();
    let (logging, _) = ssh::logging(false);
    let auth = SshAuthentication::Password {
        username: "unused".into(),
        password: "unused".into(),
    };
    let error = ssh::factory(&logging)
        .connect(host, port.parse().unwrap(), auth, None, false)
        .await
        .err()
        .unwrap();
    assert_eq!(error.reason, DisconnectReason::Unknown);
    assert!(
        error
            .message()
            .unwrap()
            .starts_with("Unable to reach a settlement of KeyExchangeAlgorithms: ["),
        "{error}"
    );
    assert!(!is_transient_connection_failure(&ConnectionError::Device(
        DeviceFailureException::new(DeviceFailureReason::UnreachableHost, None, Some(error))
    )));
}
