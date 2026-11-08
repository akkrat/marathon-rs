//! Opt-in runs against a real OpenSSH server (orchestrator decision 5 and the real-host measurements): the generic
//! executor suite, and T04, T07, T16, T30 and T32 as Linux and macOS OpenSSH profiles. Ignored by default; set
//! `MARATHON_SSH_REAL=<host>:<port>:<user>:<private key path>` (a throwaway sshd, see docs/wp/4.2-findings.md) and
//! run `cargo test -p marathon-apple --test ssh_real_sshd -- --ignored --test-threads 1`. The D35 case also needs
//! `MARATHON_SSH_REAL_NO_KEX`, see its comment.
mod support;
use marathon_apple::{
    cmd::{
        CommandExecutor, CommandRequest, CommandSession, consume_output,
        remote::ssh::sshj::{DisconnectReason, SshAuthentication, SshCommandExecutor, channel::Stream},
    },
    device::{connection_factory::ConnectionError, connection_failures::is_transient_connection_failure},
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
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
        let survives = ssh::run(&executor, &["echo", "hello"], 10_000)
            .await
            .map(|r| r.stdout.clone());
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
        .map(|f| (f.class.to_owned(), f.message.as_ref().map(|m| m.to_owned())))
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

/// U154 on a real sshd, which answers an `exec` only when its session process runs, so that process is stopped to stall the
/// request. It is stopped after the channel has opened, since an open has its own wait. The whole connection stalls with it,
/// so neither the reply nor the answer to CLOSE comes, and a second connection resumes it. It then starts the command it had
/// queued and takes the CLOSE, which is the end of that command's input: `cat` ends, and sshd gives the slot back. Without the
/// CLOSE, `cat` waits on its input and holds the slot until the connection ends. A command that does not read its input keeps
/// running: see the x21 findings.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn u154_an_exec_with_no_reply_is_closed_when_the_peer_resumes() {
    let (stalled, other) = (connect().await, connect().await);
    // The nearest sshd above the shell handles this connection's packets, with or without privilege separation.
    let find = "p=$$; while [ \"$p\" -gt 1 ]; do case \"$(ps -o comm= -p \"$p\")\" in *sshd*) echo \"$p\"; exit 0;; esac; p=$(ps -o ppid= -p \"$p\" | tr -d \" \"); done; exit 1";
    let found = ssh::run(&stalled, &["sh", "-c", find], 10_000).await.unwrap();
    assert_eq!(found.exit_code, Some(0), "{found:?}");
    let pid = found.stdout[0].clone();
    // This run's own text, because the probe below matches command lines.
    let mark = format!("u154-{}", std::process::id());
    let channel = stalled.connection().start_session().await.unwrap();
    assert_eq!(
        ssh::run(&other, &["kill", "-STOP", pid.as_str()], 10_000)
            .await
            .unwrap()
            .exit_code,
        Some(0)
    );
    let started = Instant::now();
    let result = channel.exec(format!("sh -c 'cat; echo {mark}'")).await;
    let waited_ms = started.elapsed().as_millis();
    // Before any assertion: a failed one must not leave the session process stopped.
    let resumed = ssh::run(&other, &["kill", "-CONT", pid.as_str()], 10_000)
        .await
        .unwrap();
    assert_eq!(resumed.exit_code, Some(0));
    // No reply in 30 s, then no answer to CLOSE in 5 s.
    assert!(waited_ms >= 35_000, "{waited_ms}");
    let error = format!("{:?}", result.err().unwrap());
    assert!(error.contains("Timeout expired: 30000 MILLISECONDS"), "{error}");
    let first = format!("[{}]{}", &mark[..1], &mark[1..]);
    let probe = format!("pgrep -fl \"{first}\"");
    let left = ["sh", "-c", probe.as_str()];
    let mut found = Vec::new();
    for _ in 0..100 {
        found = ssh::run(&other, &left, 10_000).await.unwrap().stdout;
        if found.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !found.is_empty() {
        let kill = format!("pkill -f \"{first}\"");
        let _ = ssh::run(&other, &["sh", "-c", kill.as_str()], 10_000).await;
    }
    assert_eq!(
        found,
        Vec::<String>::new(),
        "the late command is still waiting on its input"
    );
    // The connection carries on.
    assert_eq!(
        ssh::run(&stalled, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(stalled.connected());
    println!("U154.info.waited_ms: {waited_ms}");
}

/// How many session channels this connection can hold at once, which is the worker's `MaxSessions` less any it already holds.
async fn free_slots(executor: &SshCommandExecutor) -> usize {
    let mut open = Vec::new();
    // `MaxSessions` is 10 by default, and each open past it is refused at once.
    while open.len() < 64 {
        match executor.connection().start_session().await {
            Ok(channel) => open.push(channel),
            Err(_) => break,
        }
    }
    let free = open.len();
    for channel in open {
        channel.close().await.unwrap();
    }
    free
}

/// U157 on a real sshd, with its session process stopped before the open as U154's is before the exec. When it resumes it
/// confirms the open, after the caller has gone, and only then can that channel be closed. Without that it holds a slot until
/// the connection ends, on any `MaxSessions`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn u157_an_open_with_no_reply_is_closed_when_the_peer_resumes() {
    let (stalled, other) = (connect().await, connect().await);
    let find = "p=$$; while [ \"$p\" -gt 1 ]; do case \"$(ps -o comm= -p \"$p\")\" in *sshd*) echo \"$p\"; exit 0;; esac; p=$(ps -o ppid= -p \"$p\" | tr -d \" \"); done; exit 1";
    let found = ssh::run(&stalled, &["sh", "-c", find], 10_000).await.unwrap();
    assert_eq!(found.exit_code, Some(0), "{found:?}");
    let pid = found.stdout[0].clone();
    let slots = free_slots(&stalled).await;
    assert!(slots > 0);
    assert_eq!(
        ssh::run(&other, &["kill", "-STOP", pid.as_str()], 10_000)
            .await
            .unwrap()
            .exit_code,
        Some(0)
    );
    let started = Instant::now();
    let result = stalled.connection().start_session().await;
    let waited_ms = started.elapsed().as_millis();
    // Before any assertion: a failed one must not leave the session process stopped.
    let resumed = ssh::run(&other, &["kill", "-CONT", pid.as_str()], 10_000)
        .await
        .unwrap();
    assert_eq!(resumed.exit_code, Some(0));
    // The open's own 30 s, as in Kotlin: nothing can be closed yet, so nothing more is waited for.
    assert!(waited_ms >= 30_000, "{waited_ms}");
    let error = format!("{:?}", result.err().unwrap());
    assert!(error.contains("Timeout expired: 30000 MILLISECONDS"), "{error}");
    let mut free = 0;
    for _ in 0..100 {
        free = free_slots(&stalled).await;
        if free == slots {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(free, slots, "the late channel still holds its slot");
    assert_eq!(
        ssh::run(&stalled, &["echo", "hello"], 10_000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(stalled.connected());
    println!("U157.info.waited_ms: {waited_ms} slots: {slots}");
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d36_a_killed_server_session_makes_safe_execute_return_null() {
    // Ends this connection's own sshd session process, the nearest one above the command, as a dropping server does.
    let executor = connect().await;
    let script = "p=$PPID; while [ \"$p\" -gt 1 ]; do case \"$(ps -o comm= -p $p)\" in *sshd*) kill -9 $p; break;; esac; p=$(ps -o ppid= -p $p | tr -d \" \"); done";
    let _ = executor
        .safe_execute(request(&["sh", "-c", script], 10_000), context())
        .await;
    let started = Instant::now();
    while executor.connected() && started.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!executor.connected());
    for _ in 0..3 {
        assert_eq!(
            executor
                .safe_execute(request(&["echo", "hello"], 5000), context())
                .await,
            Ok(None)
        );
    }
}

/// A flood of `rounds` MiB on stdout, a stderr byte after each, then exit 7.
fn flood_script(rounds: u32) -> String {
    format!("i=0; while [ $i -lt {rounds} ]; do yes | head -c 1048576; printf e >&2; i=$((i+1)); done; exit 7")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d37_reading_only_stderr_keeps_the_connection_on_a_real_sshd() {
    // The scenario of fixtures/oracle/ssh/flood.jsonl on OpenSSH: stderr only, then the other session, then stdout.
    use tokio::io::AsyncReadExt;
    const CAP: usize = 16 * 1024 * 1024;
    const ADJUST_THRESHOLD: u32 = 512 * 1024;
    let executor = connect().await;
    let channel = executor.connection().start_session().await.unwrap();
    channel.exec(flood_script(32)).await.unwrap();
    let mut stderr = channel.reader(Stream::Stderr);
    let mut err = 0;
    let started = Instant::now();
    while executor.connected() && started.elapsed() < Duration::from_secs(60) {
        let window = channel.local_win_size();
        if window <= ADJUST_THRESHOLD && channel.available(Stream::Stderr) > 0 {
            err += stderr.read(&mut [0u8; 1]).await.unwrap();
        } else if window == 0 {
            break;
        } else {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    assert!(executor.connected(), "the connection must survive");
    assert_eq!(channel.local_win_size(), 0, "not blocked within 60 s");
    let buffered = channel.available(Stream::Stdout);
    assert!(buffered < CAP, "{buffered}");
    println!("D37.info.real: stderr read {err}, stdout buffered {buffered}");

    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
    assert_eq!(channel.available(Stream::Stdout), buffered);

    let mut stdout = channel.reader(Stream::Stdout);
    let mut out = 0;
    let mut bytes = vec![0; 64 * 1024];
    let started = Instant::now();
    while (out < 32 * 1024 * 1024 || err < 32) && started.elapsed() < Duration::from_secs(60) {
        if channel.available(Stream::Stdout) > 0 {
            out += stdout.read(&mut bytes).await.unwrap();
        } else if channel.available(Stream::Stderr) > 0 {
            err += stderr.read(&mut bytes).await.unwrap();
        } else {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    assert_eq!((out, err), (32 * 1024 * 1024, 32));
    tokio::time::timeout(Duration::from_secs(60), channel.close_escalating())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(channel.exit_status(), Some(7));
    assert!(executor.connected());
}

/// With stderr every 64 KiB, rarer than which a stderr read grants nothing and the session stays on the initial window
/// (see `flood_reaching_the_cap` in `ssh_session.rs`). More than the cap in all.
const FLOOD_REACHING_THE_CAP: &str = "i=0; while [ $i -lt 512 ]; do yes | head -c 65536; echo e >&2; i=$((i+1)); done";

/// A session on `executor` whose stdout consumer has stopped for good, and that has reached the cap because of it.
async fn stuck_at_the_cap(
    executor: &SshCommandExecutor,
    idle_millis: Option<i64>,
    context: marathon_core::vendor::VendorContext,
) -> (
    Arc<CommandSession>,
    tokio::task::JoinHandle<marathon_core::error::VendorResult<Option<i32>>>,
) {
    let mut flood = request(&["sh", "-c", FLOOD_REACHING_THE_CAP], 600_000);
    if let Some(idle) = idle_millis {
        flood.idle_timeout = duration(idle);
    }
    let session = Arc::new(executor.execute(flood, context.clone()).await.unwrap());
    let consumed = session.clone();
    let consumer = tokio::spawn(async move {
        consume_output(
            &consumed.base,
            &context,
            |_| std::future::pending(),
            |_| std::future::ready(Ok(())),
        )
        .await
    });
    ssh::wait_for_the_cap(executor).await;
    (session, consumer)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d37_at_the_cap_other_sessions_flow_and_the_command_timeout_ends_it_on_a_real_sshd() {
    let executor = connect().await;
    let expire = Arc::new(tokio::sync::Notify::new());
    let gated = marathon_core::vendor::VendorContext {
        clock: Arc::new(ssh::GatedTimeoutClock::new(expire.clone())),
        ..context()
    };
    let (session, consumer) = stuck_at_the_cap(&executor, None, gated).await;
    // The blocked channel holds nothing shared: another command on this connection runs while it stays blocked.
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
    expire.notify_one();
    let exit = tokio::time::timeout(Duration::from_secs(60), session.base.exit_code.wait())
        .await
        .unwrap();
    assert_eq!(exit, Ok(None));
    consumer.abort();
    session.close().await.unwrap();
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d37_at_the_cap_the_idle_timeout_ends_it_on_a_real_sshd() {
    let executor = connect().await;
    let (session, consumer) = stuck_at_the_cap(&executor, Some(5000), context()).await;
    let error = tokio::time::timeout(Duration::from_secs(60), consumer)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.failure().class.to_owned(),
        "java.util.concurrent.TimeoutException"
    );
    session.close().await.unwrap();
    assert!(executor.connected());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d37_at_the_cap_close_returns_on_a_real_sshd() {
    let executor = connect().await;
    let (session, consumer) = stuck_at_the_cap(&executor, None, context()).await;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(60), session.close())
        .await
        .unwrap()
        .unwrap();
    println!("D37.info.real_close_ms: {}", started.elapsed().as_millis());
    consumer.abort();
    assert!(executor.connected());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs MARATHON_SSH_REAL"]
async fn d37_at_the_cap_kill_ends_a_process_that_ignores_term_on_a_real_sshd() {
    // D30 after D37: the close waits its 20 s, KILL goes out with the window closed, and the close still succeeds.
    let executor = connect().await;
    let script = format!("trap \"\" TERM; {FLOOD_REACHING_THE_CAP}");
    let session = Arc::new(
        executor
            .execute(request(&["sh", "-c", &script], 600_000), context())
            .await
            .unwrap(),
    );
    let consumed = session.clone();
    let consumer = tokio::spawn(async move {
        consume_output(
            &consumed.base,
            &context(),
            |_| std::future::pending(),
            |_| std::future::ready(Ok(())),
        )
        .await
    });
    ssh::wait_for_the_cap(&executor).await;
    let started = Instant::now();
    let closed = tokio::time::timeout(Duration::from_secs(60), session.close())
        .await
        .unwrap();
    let close_ms = started.elapsed().as_millis();
    println!("D37.info.real_kill_close_ms: {close_ms}");
    assert_eq!(closed, Ok(()));
    assert!((19_000..29_000).contains(&close_ms), "{close_ms}");
    consumer.abort();
    assert!(executor.connected());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
}
