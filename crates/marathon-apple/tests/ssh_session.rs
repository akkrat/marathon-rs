//! Execute, streams, exit wait, close and keep-alive of the SSH executor on the fake worker: the spike checklist
//! (T08, T09, T14-T16, T22-T25), D7, D29, D30, D32, A2, A11, A14, A32, approved P2/P5 and shared-connection flow control.
mod support;
use fake_worker::script::{Action, Chunk, Faults, Keepalive};
use jcompat::logging::Level;
use marathon_apple::cmd::{
    CommandExecutor, CommandRequest, CommandSession, cancelled,
    remote::ssh::sshj::{
        SshCommandExecutor, SshFailure,
        channel::{ExecChannel, Stream},
        client::{MAX_PACKET_SIZE, WINDOW_SIZE},
        error::*,
        executor::exec_string,
    },
};
use marathon_core::error::{Failure, VendorError};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use support::{context, duration, ssh};

fn request(command: &[&str], timeout: i64) -> CommandRequest {
    CommandRequest::new(command.iter().copied(), duration(timeout))
}
async fn start(executor: &SshCommandExecutor, command: &[&str], timeout: i64) -> CommandSession {
    executor.execute(request(command, timeout), context()).await.unwrap()
}
fn class_chain(failure: &Failure) -> Vec<(String, Option<String>)> {
    std::iter::successors(Some(failure), |f| f.cause.as_deref())
        .map(|f| {
            (
                f.class.to_string_lossy(),
                f.message.as_ref().map(|m| m.to_string_lossy()),
            )
        })
        .collect()
}
fn other(error: VendorError) -> Failure {
    match error {
        VendorError::Other(failure) => failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}
fn hang_with(chunks: Vec<Chunk>, ignore: &[&str], withhold_close: bool) -> Action {
    Action::Output {
        chunks,
        status: 0,
        signal: None,
        hang: ignore.is_empty(),
        ignore_signals: ignore.iter().map(|s| s.to_string()).collect(),
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close,
    }
}
/// A process that TERM does not end, or nothing does, on a peer that answers CLOSE or never does.
fn unstoppable(ignore: &[&str], withhold_close: bool) -> Action {
    Action::Output {
        chunks: vec![],
        status: 0,
        signal: None,
        hang: true,
        ignore_signals: ignore.iter().map(|s| s.to_string()).collect(),
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close,
    }
}
fn signal_names(signals: &[serde_json::Value]) -> Vec<&str> {
    signals
        .iter()
        .map(|s| s["detail"]["signal"].as_str().unwrap())
        .collect()
}
fn signalled(signal: &str) -> Action {
    Action::Output {
        chunks: vec![],
        status: 0,
        signal: Some(signal.into()),
        hang: false,
        ignore_signals: vec![],
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: false,
    }
}
fn rules(extra: Vec<fake_worker::script::Rule>) -> fake_worker::script::Script {
    let mut script = ssh::suite_script();
    script.rules.splice(0..0, extra);
    script
}
async fn executor_with(fake: &ssh::Fake, factor: i64, keep_alive_millis: i64) -> (SshCommandExecutor, ssh::Events) {
    let (logging, events) = ssh::logging(false);
    let mut factory = ssh::scaled_factory(&logging, factor);
    factory.keep_alive_interval = duration(keep_alive_millis);
    factory.keep_alive_max_count = 2;
    (ssh::connect_with(fake, &factory, false).await.unwrap(), events)
}

async fn wait_for_channel_state(channel: &ExecChannel, what: &str, done: impl Fn() -> bool) {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "{what} within 30 s: window {}, stdout {}, stderr {}",
        channel.local_win_size(),
        channel.available(Stream::Stdout),
        channel.available(Stream::Stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn open_failure_is_a_connection_exception_and_safe_execute_returns_null() {
    // T08: MaxSessions answers with an open failure (code 4); the connection stays usable.
    let fake = ssh::fake(ssh::suite_script(), 1).await;
    let (executor, events) = executor_with(&fake, 1, 0).await;
    let held = start(&executor, &["sleep", "1000"], 600_000).await;
    let failure = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert_eq!(
        class_chain(&failure),
        [(OPEN_FAIL_EXCEPTION.into(), Some("Resource shortage".into()))]
    );
    assert_eq!(
        executor
            .safe_execute(request(&["echo", "hello"], 5000), context())
            .await,
        Ok(None)
    );
    let lines = ssh::lines(&events);
    assert!(lines.contains(&(
        "Promise".into(),
        Level::Error,
        "<<chan#1 / open>> woke to: Opening `session` channel failed: Resource shortage".into()
    )));
    let last = events.lock().unwrap().last().cloned().unwrap();
    let (_, _, message, throwable) = &last;
    assert_eq!(message, "Unable to start a remote shell session [echo, hello]");
    assert_eq!(throwable.as_ref().unwrap().class, OPEN_FAIL_EXCEPTION.into());
    held.terminate().unwrap();
    held.close().await.unwrap();
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_rejection_is_request_failed_and_closes_the_channel_d29() {
    let fake = ssh::fake(
        fake_worker::script::Script {
            faults: Faults {
                reject_exec: true,
                ..Default::default()
            },
            ..ssh::suite_script()
        },
        1,
    )
    .await;
    let (executor, events) = executor_with(&fake, 1, 0).await;
    let failure = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    // T23: ConnectionException[UNKNOWN]("Request failed"); the connection stays usable.
    assert_eq!(
        class_chain(&failure),
        [(CONNECTION_EXCEPTION.into(), Some("Request failed".into()))]
    );
    assert!(executor.connected());
    assert!(ssh::lines(&events).contains(&(
        "Promise".into(),
        Level::Error,
        "<<chan#0 / chanreq for exec>> woke to: net.schmizz.sshj.connection.ConnectionException: Request failed".into()
    )));
    // D29 (A33): Kotlin never closes the rejected channel, so with MaxSessions 1 the next command could not open one
    // (the Kotlin "reject" transcript has no client_close). Here the slot is free, and the worker rejects it in turn.
    for _ in 0..3 {
        let next = other(
            executor
                .execute(request(&["echo", "hello"], 5000), context())
                .await
                .err()
                .unwrap(),
        );
        assert_eq!(
            class_chain(&next),
            [(CONNECTION_EXCEPTION.into(), Some("Request failed".into()))]
        );
    }
    assert_eq!(fake.events_at_least("client_close", 4).await.len(), 4);
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_rejection_by_a_peer_that_never_answers_close_is_still_request_failed_d29() {
    let fake = ssh::fake(
        fake_worker::script::Script {
            faults: Faults {
                reject_exec: true,
                withhold_close: true,
                ..Default::default()
            },
            ..ssh::suite_script()
        },
        4,
    )
    .await;
    // 5 s in compressed time (factor 10).
    let (executor, events) = executor_with(&fake, 10, 0).await;
    let started = Instant::now();
    let failure = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    // The bound on the CLOSE does not replace the rejection, and only the rejection's own line is logged.
    assert_eq!(
        class_chain(&failure),
        [(CONNECTION_EXCEPTION.into(), Some("Request failed".into()))]
    );
    let woke: Vec<_> = ssh::lines(&events)
        .into_iter()
        .filter(|(logger, ..)| logger == "Promise")
        .map(|(.., message)| message)
        .collect();
    assert_eq!(
        woke,
        ["<<chan#0 / chanreq for exec>> woke to: net.schmizz.sshj.connection.ConnectionException: Request failed"]
    );
    assert_eq!(fake.events_at_least("client_close", 1).await.len(), 1);
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn env_and_workdir_are_ignored() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    let mut request = request(&["echo", "hello"], 5000);
    request.env = BTreeMap::from([("F".into(), "1".into())]);
    request.workdir = Some("/tmp".into());
    let session = executor.execute(request, context()).await.unwrap();
    assert_eq!(session.await_result(&context()).await.unwrap().stdout, ["hello"]);
    session.close().await.unwrap();
    // A2: the exec string is the command alone, and no env request is sent.
    let events = fake.transcript().await;
    let exec: Vec<_> = events.iter().filter(|e| e["event"] == "exec").collect();
    assert_eq!(exec[0]["detail"]["base64"], "ZWNobyBoZWxsbw==");
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == "request" && e["detail"]["kind"] == "env")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_ns_unbuffered_io_reaches_the_worker_in_the_exec_string_d32() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    let variants = [
        (
            &[("NSUnbufferedIO", "YES"), ("F", "1")][..],
            "env NSUnbufferedIO=YES echo hello",
        ),
        (&[("NSUnbufferedIO", "NO")][..], "echo hello"),
        (&[("SIMCTL_CHILD_A", "YES")][..], "echo hello"),
    ];
    for (env, wire) in variants {
        let mut request = request(&["echo", "hello"], 5000);
        request.env = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        request.workdir = Some("/tmp".into());
        assert_eq!(exec_string(&request), wire);
    }
    let mut unbuffered = request(&["sh", "-c", "echo $0"], 5000);
    unbuffered.env = BTreeMap::from([("NSUnbufferedIO".into(), "YES".into())]);
    // The quoting of what follows the prefix is the command's own.
    assert_eq!(exec_string(&unbuffered), "env NSUnbufferedIO=YES sh -c 'echo $0'");
    let mut rejected = unbuffered.clone();
    rejected.command = vec!["echo".into(), "hello".into()];
    let session = executor.execute(rejected, context()).await.unwrap();
    let _ = session.await_result(&context()).await;
    session.close().await.unwrap();
    let events = fake.transcript().await;
    let exec: Vec<_> = events.iter().filter(|e| e["event"] == "exec").collect();
    assert_eq!(
        exec[0]["detail"]["base64"],
        "ZW52IE5TVW5idWZmZXJlZElPPVlFUyBlY2hvIGhlbGxv"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == "request" && e["detail"]["kind"] == "env")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_lines_line_endings_and_data_after_eof() {
    let fake = ssh::fake(
        rules(vec![
            ssh::exact("partial", ssh::output(vec![ssh::chunk("a\nb\npartial")], 3)),
            ssh::exact("endings", ssh::output(vec![ssh::chunk("x\r\ny\rz\n\n\r\nw")], 0)),
            ssh::exact(
                "late",
                ssh::output(
                    vec![
                        ssh::chunk("early\n"),
                        Chunk {
                            data: "late\n".into(),
                            eof_before: true,
                            ..Default::default()
                        },
                    ],
                    0,
                ),
            ),
        ]),
        4,
    )
    .await;
    let executor = ssh::connect(&fake).await;
    // T14: the unterminated tail is drained at EOF; CR, LF and CRLF all end a line.
    let partial = ssh::run(&executor, &["partial"], 5000).await.unwrap();
    assert_eq!(
        (partial.stdout, partial.exit_code),
        (vec!["a".to_owned(), "b".into(), "partial".into()], Some(3))
    );
    assert_eq!(
        ssh::run(&executor, &["endings"], 5000).await.unwrap().stdout,
        ["x", "y", "z", "", "", "w"]
    );
    // PatchedExecChannel: data after EOF is dropped silently.
    let late = ssh::run(&executor, &["late"], 5000).await.unwrap();
    assert_eq!((late.stdout, late.exit_code), (vec!["early".to_owned()], Some(0)));
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_extended_data_type_kills_the_connection() {
    let fake = ssh::fake(
        rules(vec![ssh::exact(
            "bad",
            ssh::output(
                vec![Chunk {
                    data: "x".into(),
                    extended: Some(42),
                    ..Default::default()
                }],
                0,
            ),
        )]),
        4,
    )
    .await;
    let (executor, events) = executor_with(&fake, 1, 0).await;
    let other_session = start(&executor, &["sleep", "1000"], 600_000).await;
    let error = other(ssh::run(&executor, &["bad"], 5000).await.err().unwrap());
    let lost = vec![(
        CONNECTION_EXCEPTION.to_owned(),
        Some("Bad extended data type = 42".to_owned()),
    )];
    assert_eq!(class_chain(&error), lost);
    // The reader thread dies, so every channel of the connection fails with it.
    let error = other(other_session.await_result(&context()).await.err().unwrap());
    assert_eq!(class_chain(&error), lost);
    assert!(!executor.connected());
    assert!(ssh::lines(&events).contains(&(
        "TransportImpl".into(),
        Level::Error,
        "Dying because - Bad extended data type = 42".into()
    )));
    let disconnect: Vec<_> = fake.events_at_least("connection_end", 1).await;
    assert_eq!(disconnect.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn transport_loss_fails_an_in_flight_await() {
    let fake = ssh::fake(rules(vec![ssh::exact("drop", Action::DropTransport)]), 4).await;
    let (executor, events) = executor_with(&fake, 1, 0).await;
    let session = start(&executor, &["sleep", "1000"], 600_000).await;
    let _ = executor.execute(request(&["drop"], 5000), context()).await;
    // R2-wonder-3: the readers end normally; the transport error arrives through the exit wait.
    let error = other(session.await_result(&context()).await.err().unwrap());
    let chain = class_chain(&error);
    assert_eq!(chain[0].0, CONNECTION_EXCEPTION);
    assert_eq!(chain[1].0, TRANSPORT_EXCEPTION);
    assert!(!executor.connected());
    assert!(
        ssh::lines(&events)
            .iter()
            .any(|(logger, _, message)| logger == "TransportImpl" && message.starts_with("Dying because - "))
    );
    let _ = session.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn non_term_signal_exit_sends_no_stale_term() {
    let fake = ssh::fake(
        rules(vec![
            ssh::exact("killed", signalled("KILL")),
            ssh::exact("segv", signalled("SEGV")),
        ]),
        4,
    )
    .await;
    let executor = ssh::connect(&fake).await;
    for command in ["killed", "segv"] {
        // T16: null exit code; D7: no TERM request follows on the closed channel, the connection survives.
        assert_eq!(ssh::run(&executor, &[command], 5000).await.unwrap().exit_code, None);
    }
    assert!(!fake.transcript().await.iter().any(|e| e["event"] == "signal"));
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn await_cancellation_kills_nothing_and_is_alive_stays_true() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    let session = start(&executor, &["sleep", "1000"], 600_000).await;
    let ctx = context();
    let scope = Arc::new(marathon_apple::cmd::SessionScope::default());
    let cancellable = marathon_core::vendor::VendorContext {
        cancellation: scope.clone(),
        ..ctx
    };
    let (result, ()) = tokio::join!(session.await_result(&cancellable), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        scope.cancel(cancelled("await cancelled"));
    });
    // T22: cancellation only; the remote command still runs and the executor stays usable.
    assert!(matches!(result, Err(VendorError::Cancelled(_))));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = fake.transcript().await;
    assert!(
        !events
            .iter()
            .any(|e| e["event"] == "signal" || e["event"] == "client_close")
    );
    assert!(session.is_alive());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
    session.close().await.unwrap();
    // A11: isAlive was captured at construction and stays true after the command has ended.
    let finished = start(&executor, &["echo", "hello"], 5000).await;
    finished.await_result(&context()).await.unwrap();
    assert!(finished.is_alive());
    finished.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn await_concurrent_with_close_is_cancelled() {
    // Approved P2 (T07/T14): an await still running when close() finishes its join observes cancellation.
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    for _ in 0..20 {
        let session = start(&executor, &["sleep", "1000"], 600_000).await;
        let ctx = context();
        let (result, closed) = tokio::join!(session.await_result(&ctx), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            session.close().await
        });
        assert!(matches!(result, Err(VendorError::Cancelled(_))), "{result:?}");
        assert_eq!(closed, Ok(()));
    }
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn first_close_fails_after_term_kill_and_thirty_seconds_and_second_close_cancels_the_scope_d30() {
    // T15 with sshj's 30 s in compressed time (factor 10): nothing ends the process, and the peer never answers CLOSE.
    let fake = ssh::fake(rules(vec![ssh::exact("held", unstoppable(&["TERM", "KILL"], true))]), 4).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    let session = start(&executor, &["held"], 600_000).await;
    let started = Instant::now();
    let first = other(session.close().await.err().unwrap());
    assert!(
        started.elapsed() >= Duration::from_millis(3000),
        "{:?}",
        started.elapsed()
    );
    // A14: ConnectionException wrapping TimeoutException (Kotlin evidence t15.stdout).
    let expected = vec![
        (
            CONNECTION_EXCEPTION.to_owned(),
            Some("Timeout expired: 30000 MILLISECONDS".to_owned()),
        ),
        (
            TIMEOUT_EXCEPTION.to_owned(),
            Some("Timeout expired: 30000 MILLISECONDS".to_owned()),
        ),
    ];
    assert_eq!(class_chain(&first), expected);
    // D30: Kotlin sends one TERM before CLOSE and no KILL. KILL goes before CLOSE, which russh ends the channel with.
    let transcript = fake.transcript().await;
    let order: Vec<_> = ssh::channel_events(&transcript, 1)
        .into_iter()
        .map(|(_, event)| event)
        .filter(|event| event == "signal" || event == "client_close")
        .collect();
    assert_eq!(order, ["signal", "signal", "client_close"]);
    assert_eq!(signal_names(&fake.events("signal").await), ["TERM", "KILL"]);
    let lines = ssh::lines(&events);
    let abandoned = lines
        .iter()
        .filter(|(_, level, message)| *level == Level::Error && message.starts_with("Abandoning session `held`:"))
        .count();
    assert_eq!(abandoned, 1, "{lines:?}");
    // Approved P5: the second close skips the backend (Kotlin's closed flag) but cancels and joins the scope.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(60), session.close()).await,
        Ok(Ok(()))
    );
    assert!(matches!(
        session.await_result(&context()).await,
        Err(VendorError::Cancelled(_))
    ));
    assert_eq!(fake.events("signal").await.len(), 2);
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fast_exit_on_term_closes_at_once_and_sends_no_kill_d30() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    // At this scale KILL would come after a real 20 s and show in the signals below; the deadline only catches a hang.
    let (executor, events) = executor_with(&fake, 1, 0).await;
    let session = start(&executor, &["sleep", "1000"], 600_000).await;
    let closed = tokio::time::timeout(Duration::from_secs(60), session.close()).await;
    assert_eq!(closed, Ok(Ok(())));
    let signals = fake.events_at_least("signal", 1).await;
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0]["detail"]["signal"], "TERM");
    assert!(
        ssh::lines(&events)
            .iter()
            .all(|(.., message)| !message.starts_with("Abandoning"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_ends_a_process_that_ignores_term_and_the_close_succeeds_d30() {
    let fake = ssh::fake(rules(vec![ssh::exact("stubborn", unstoppable(&["TERM"], false))]), 4).await;
    let (executor, events) = executor_with(&fake, 5, 0).await;
    let session = start(&executor, &["stubborn"], 600_000).await;
    let started = Instant::now();
    assert_eq!(session.close().await, Ok(()));
    // 20 s at this scale: the process had its time to end on TERM. The reply then has 2 s.
    assert!(
        started.elapsed() >= Duration::from_millis(4000),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(signal_names(&fake.events("signal").await), ["TERM", "KILL"]);
    assert_eq!(
        fake.events_at_least("exit_signal", 1).await[0]["detail"]["signal"],
        "KILL"
    );
    assert!(
        ssh::lines(&events)
            .iter()
            .all(|(.., message)| !message.starts_with("Abandoning"))
    );
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exit_report_ends_the_term_wait_when_the_peer_keeps_the_channel_open_d30() {
    // sshd can report an exit and hold its CLOSE until the client's, for example while a child holds the output open.
    // At factor 1 the TERM wait lasts 20 s: if the report did not end it, KILL would come before CLOSE.
    let fake = ssh::fake(rules(vec![ssh::exact("held", hang_with(vec![], &[], true))]), 4).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    let session = Arc::new(start(&executor, &["held"], 600_000).await);
    let closing = session.clone();
    let close = tokio::spawn(async move { closing.close().await });
    tokio::time::timeout(Duration::from_secs(60), async {
        while fake.events("client_close").await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(signal_names(&fake.events("signal").await), ["TERM"]);
    close.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_awaiter_of_an_abandoned_session_is_released_without_a_second_close_d30() {
    let fake = ssh::fake(rules(vec![ssh::exact("held", unstoppable(&["TERM", "KILL"], true))]), 4).await;
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let session = start(&executor, &["held"], 600_000).await;
    let ctx = context();
    let both = async { tokio::join!(session.await_result(&ctx), session.close()) };
    let (awaited, closed) = tokio::time::timeout(Duration::from_secs(60), both).await.unwrap();
    assert!(closed.is_err());
    assert!(matches!(awaited, Err(VendorError::Cancelled(_))), "{awaited:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn abandoning_a_session_leaves_the_connection_and_its_other_sessions_alone_d30() {
    let fake = ssh::fake(rules(vec![ssh::exact("held", unstoppable(&["TERM", "KILL"], true))]), 2).await;
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let other_session = start(&executor, &["sleep", "1000"], 600_000).await;
    let session = start(&executor, &["held"], 600_000).await;
    assert!(session.close().await.is_err());
    assert_eq!(session.close().await, Ok(()));
    // The session is gone locally, and the other one and the connection are untouched.
    assert!(executor.connected());
    assert_eq!(fake.events("connection_end").await.len(), 0);
    assert_eq!(other_session.close().await, Ok(()));
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_close_failure_fails_the_exit_code_and_safe_execute_returns_null() {
    let fake = ssh::fake(rules(vec![ssh::exact("held", unstoppable(&["TERM", "KILL"], true))]), 4).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    // T15h: the total timeout sends TERM, D30's close waits 30 s and fails, so await fails instead of returning null.
    let session = start(&executor, &["held"], 100).await;
    let failure = other(session.await_result(&context()).await.err().unwrap());
    assert_eq!(
        class_chain(&failure)[0],
        (
            CONNECTION_EXCEPTION.into(),
            Some("Timeout expired: 30000 MILLISECONDS".into())
        )
    );
    // D30: Kotlin's later session close joins without bound. Here the timeout branch has already given up, so the close
    // fails at once with no new signal, and the one after it (approved P5) cancels and joins the scope.
    assert_eq!(fake.events_at_least("signal", 2).await.len(), 2);
    let later = other(
        tokio::time::timeout(Duration::from_secs(60), session.close())
            .await
            .expect("the later close is bounded")
            .err()
            .unwrap(),
    );
    assert_eq!(later.class, CONNECTION_EXCEPTION.into());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(60), session.close()).await,
        Ok(Ok(()))
    );
    assert_eq!(fake.events("signal").await.len(), 2);
    // safeExecute turns the same failure into null and logs it with its throwable.
    assert_eq!(
        executor.safe_execute(request(&["held"], 100), context()).await,
        Ok(None)
    );
    let last = events.lock().unwrap().last().cloned().unwrap();
    let (_, level, message, throwable) = &last;
    assert_eq!(
        (*level, message.as_str()),
        (Level::Error, "Unable to start a remote shell session [held]")
    );
    assert_eq!(throwable.as_ref().unwrap().class, CONNECTION_EXCEPTION.into());
    assert!(executor.connected());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ignored_term_on_timeout_still_gives_null() {
    // T15t: TERM is ignored and the result is null. sshd answers the CLOSE only once the process ends (~8 s on
    // the container); the fake's russh server answers at once, so the wait itself is not observable here.
    let late = Chunk {
        data: "late\n".into(),
        delay_ms: 400,
        ..Default::default()
    };
    let fake = ssh::fake(
        rules(vec![ssh::exact("stubborn", hang_with(vec![late], &["TERM"], false))]),
        4,
    )
    .await;
    // The reply to CLOSE has a real second at this factor, and the fake's late output comes well within the TERM wait.
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let result = ssh::run(&executor, &["stubborn"], 100).await.unwrap();
    assert_eq!(result.exit_code, None);
    assert_eq!(fake.events_at_least("signal_ignored", 1).await.len(), 1);
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

fn flowing(seconds: u64) -> Action {
    ssh::output(
        (0..seconds * 10)
            .map(|i| Chunk {
                data: format!("line {i}\n"),
                delay_ms: 100,
                ..Default::default()
            })
            .collect(),
        0,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_alive_loss_fails_the_in_flight_await_while_output_flows() {
    // T25: replies withheld while output flows; channel traffic does not count as an answer (R7).
    let fake = ssh::fake(
        fake_worker::script::Script {
            faults: Faults {
                keepalive: Keepalive::Withhold,
                ..Default::default()
            },
            ..rules(vec![ssh::exact("flowing", flowing(10))])
        },
        4,
    )
    .await;
    let (executor, events) = executor_with(&fake, 1, 1000).await;
    let started = Instant::now();
    let error = other(ssh::run(&executor, &["flowing"], 12_000).await.err().unwrap());
    let elapsed = started.elapsed();
    // Kotlin evidence keepalive-with-output: 3018 ms, ConnectionException[CONNECTION_LOST].
    assert!(
        (Duration::from_millis(2500)..Duration::from_millis(4500)).contains(&elapsed),
        "{elapsed:?}"
    );
    let message = "Did not receive any keep-alive response for 2 seconds";
    assert_eq!(
        class_chain(&error),
        [(CONNECTION_EXCEPTION.into(), Some(message.into()))]
    );
    assert!(!executor.connected());
    assert!(ssh::lines(&events).contains(&(
        "TransportImpl".into(),
        Level::Error,
        format!("Dying because - {message}")
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn delayed_keep_alive_replies_keep_the_connection() {
    let fake = ssh::fake(
        fake_worker::script::Script {
            faults: Faults {
                keepalive: Keepalive::Delay(500),
                ..Default::default()
            },
            ..rules(vec![ssh::exact("flowing", flowing(4))])
        },
        4,
    )
    .await;
    let (executor, _) = executor_with(&fake, 1, 1000).await;
    let result = ssh::run(&executor, &["flowing"], 12_000).await.unwrap();
    assert_eq!((result.stdout.len(), result.exit_code), (40, Some(0)));
    assert!(executor.connected());
    assert!(fake.events("keepalive").await.len() >= 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_consumer_blocks_only_its_own_channel() {
    // Flow control (orchestrator decision 1): 16 MiB on one channel nobody reads, another channel and the
    // keep-alive keep working; the stalled channel completes once it is consumed.
    let line = format!("{}\n", "x".repeat(1023));
    let flood = ssh::output(vec![ssh::chunk(&line.repeat(16 * 1024))], 0);
    let fake = ssh::fake(
        rules(vec![ssh::exact("flood", flood), ssh::exact("flowing", flowing(4))]),
        4,
    )
    .await;
    let (executor, _) = executor_with(&fake, 1, 1000).await;
    let stalled = start(&executor, &["flood"], 600_000).await;
    let initial_packets = (WINDOW_SIZE / MAX_PACKET_SIZE) as usize;
    assert!(fake.events_at_least("output", initial_packets).await.len() >= initial_packets);
    let result = ssh::run(&executor, &["flowing"], 12_000).await.unwrap();
    assert_eq!((result.stdout.len(), result.exit_code), (40, Some(0)));
    assert!(executor.connected(), "keep-alive replies still arrive");
    let flood_channel = fake.events("exec").await[0]["channel"].clone();
    assert!(
        !fake
            .transcript()
            .await
            .iter()
            .any(|e| e["event"] == "exit_status" && e["channel"] == flood_channel)
    );
    let result = stalled.await_result(&context()).await.unwrap();
    assert_eq!((result.stdout.len(), result.exit_code), (16 * 1024, Some(0)));
    stalled.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_consumer_receive_window_stays_within_the_buffer_cap() {
    // R1-sol-2: sshj caps each window adjustment by what the stream's 16 MiB buffer can still take, so one-byte
    // reads keep at most 16 MiB - 1 unread (sshj 0.39 on this script: 16 read, 16777215 available; so do we).
    use tokio::io::AsyncReadExt;
    const CAP: usize = 16 * 1024 * 1024;
    let flood = ssh::output(vec![ssh::chunk(&"x".repeat(32 * 1024 * 1024))], 0);
    let fake = ssh::fake(rules(vec![ssh::exact("flood", flood)]), 4).await;
    let executor = ssh::connect(&fake).await;
    let channel = executor.connection().start_session().await.unwrap();
    channel.exec("flood".into()).await.unwrap();
    let mut reader = channel.reader(Stream::Stdout);
    let mut consumed = 0;
    // A zero window means the pump received all granted bytes, even when the fake is slow under load.
    wait_for_channel_state(&channel, "initial window did not drain", || {
        !executor.connected() || channel.local_win_size() == 0
    })
    .await;
    assert!(executor.connected());
    assert_eq!(channel.available(Stream::Stdout), WINDOW_SIZE as usize);
    for _ in 0..16 {
        consumed += reader.read(&mut [0u8; 1]).await.unwrap();
        wait_for_channel_state(&channel, "refilled window did not drain", || {
            !executor.connected() || channel.local_win_size() == 0
        })
        .await;
        assert!(executor.connected(), "one-byte reads must not overflow the buffer");
        assert!(channel.available(Stream::Stdout) < CAP);
    }
    let buffered = channel.available(Stream::Stdout);
    executor.close().await.unwrap();
    assert_eq!(consumed, 16);
    // This also rejects a missing refill: the initial window holds only 2 MiB. The fake's output count
    // includes a part queued ahead of the wire, so measure the client's fully drained receive window.
    assert_eq!(buffered, CAP - 1, "reads should refill the buffer up to sshj's cap");
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_only_stderr_overflows_unread_stdout_and_kills_the_connection() {
    // R2-sol-1: stderr reads refill the shared window regardless of stdout, so unread stdout reaches
    // CircularBuffer's hard cap; sshj's reader thread dies with CircularBufferException and takes the whole
    // connection. Oracle: fixtures/oracle/ssh/flood.jsonl (`tools/oracle/ssh-probes/run.sh flood`), the same
    // script read through stderr only on the Kotlin factory's client. Its consumed/buffered counts are
    // timing-dependent, so they are bounded here; the failure chain and the Dying line are compared exactly.
    use tokio::io::AsyncReadExt;
    const CAP: usize = 16 * 1024 * 1024;
    let oracle: serde_json::Value = {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/ssh/flood.jsonl");
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(text.lines().next().unwrap()).unwrap()
    };
    assert_eq!(oracle["case"], "flood_stderr_only");
    assert_eq!(oracle["connected"], false);
    let mut chunks = Vec::new();
    for _ in 0..32 {
        chunks.push(ssh::chunk(&"x".repeat(1024 * 1024)));
        chunks.push(ssh::err_chunk("e"));
    }
    let fake = ssh::fake(rules(vec![ssh::exact("flood", ssh::output(chunks, 0))]), 4).await;
    let (logging, events) = ssh::logging(false);
    let executor = ssh::connect_with(&fake, &ssh::factory(&logging), false).await.unwrap();
    let channel = executor.connection().start_session().await.unwrap();
    channel.exec("flood".into()).await.unwrap();
    let mut stderr = channel.reader(Stream::Stderr);
    // A read grants credit only while the window is at or below sshj's adjust threshold (`Window.Local`:
    // min(32 KiB * 20, 2 MiB / 4)). One that returns while granted bytes are still in flight grants none, and the
    // next stderr byte then sits behind a megabyte of stdout the fake may not send: the oracle's 100 ms sleep
    // stands in for that, and on a loaded box it lost the race. Each read waits for the window instead.
    const ADJUST_THRESHOLD: u32 = 512 * 1024;
    let mut consumed = 0;
    for _ in 0..32 {
        wait_for_channel_state(&channel, "no window drain", || {
            !executor.connected() || channel.local_win_size() <= ADJUST_THRESHOLD
        })
        .await;
        if !executor.connected() {
            break;
        }
        match tokio::time::timeout(Duration::from_secs(30), stderr.read(&mut [0u8; 1])).await {
            Ok(Ok(1)) => consumed += 1,
            Ok(_) => break,
            Err(_) => panic!("stderr read stalled after {consumed} bytes"),
        }
    }
    wait_for_channel_state(&channel, "no overflow", || !executor.connected()).await;
    assert!(!executor.connected(), "the overflow must kill the whole connection");
    assert!(consumed < 32, "all 32 stderr bytes were consumed: no overflow happened");
    let buffered = channel.available(Stream::Stdout);
    assert!(buffered < CAP, "{buffered} unread bytes exceed the buffer cap");
    assert!(
        buffered > CAP / 2,
        "{buffered} unread: death came before the cap was approached"
    );
    let error = channel.close_escalating().await.err().unwrap();
    assert_eq!(error.class, CONNECTION_EXCEPTION);
    assert_eq!(
        error.cause.as_deref().map(|cause| (cause.class, cause.message())),
        Some((CIRCULAR_BUFFER_EXCEPTION, Some("Attempted overflow".into())))
    );
    let chain: Vec<(String, Option<String>)> = std::iter::successors(Some(&error), |f| f.cause.as_deref())
        .map(|f| (f.class.to_owned(), f.message()))
        .collect();
    let expected: Vec<(String, Option<String>)> = oracle["join_error"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| {
            (
                link["class"].as_str().unwrap().to_owned(),
                link["message"].as_str().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(chain, expected);
    assert!(ssh::lines(&events).contains(&(
        "TransportImpl".into(),
        Level::Error,
        "Dying because - Attempted overflow".into()
    )));
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn flood_then_close_returns_promptly() {
    // T14's 45 s Kotlin close delay: a peer that keeps output queued while the reader stops. The fake drops its
    // queued output on CLOSE, unlike sshd, so close returns at once here (see findings).
    let line = format!("{}\n", "y".repeat(1023));
    let fake = ssh::fake(
        rules(vec![ssh::exact(
            "flood",
            ssh::output(vec![ssh::chunk(&line.repeat(16 * 1024))], 0),
        )]),
        4,
    )
    .await;
    let executor = ssh::connect(&fake).await;
    let session = start(&executor, &["flood"], 600_000).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    session.close().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 5000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_debug_logs_the_exec_command_only_when_enabled() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    for debug in [false, true] {
        let (logging, events) = ssh::logging(true);
        let executor = ssh::connect_with(&fake, &ssh::factory(&logging), debug).await.unwrap();
        ssh::run(&executor, &["echo", "Hello world"], 5000).await.unwrap();
        let exec = (
            "PatchedExecChannel".to_owned(),
            Level::Debug,
            "Will request to exec `echo 'Hello world'`".to_owned(),
        );
        assert_eq!(ssh::lines(&events).contains(&exec), debug);
        executor.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn executor_close_disconnects_and_fails_live_channels() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let executor = ssh::connect(&fake).await;
    let session = start(&executor, &["sleep", "1000"], 600_000).await;
    executor.close().await.unwrap();
    assert!(!executor.connected());
    let error = other(session.await_result(&context()).await.err().unwrap());
    assert_eq!(
        class_chain(&error),
        [
            (CONNECTION_EXCEPTION.into(), Some("Disconnected".into())),
            (TRANSPORT_EXCEPTION.into(), Some("Disconnected".into())),
        ]
    );
    let error = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    // D36 is about a transport that died: using a client after its own close stays a state error, past safeExecute.
    assert_eq!(error, SshFailure::not_connected().to_failure());
    assert_eq!(
        executor
            .safe_execute(request(&["echo", "hello"], 5000), context())
            .await,
        Err(VendorError::Other(error))
    );
    let ends = fake.events_at_least("connection_end", 1).await;
    assert_eq!(ends.len(), 1);
}
