//! Execute, streams, exit wait, close and keep-alive of the SSH executor on the fake worker: the spike checklist
//! (T08, T09, T14-T16, T22-T25), D7, D29, D30, D32, U154, U157, A2, A11, A14, A32, approved P2/P5 and shared-connection flow control.
mod support;
use fake_worker::script::{Action, Chunk, Faults, Keepalive};
use jcompat::logging::Level;
use marathon_apple::cmd::{
    CommandExecutor, CommandRequest, CommandSession, cancelled, consume_output,
    remote::ssh::sshj::{
        SshCommandExecutor, SshFailure,
        channel::{ExecChannel, Stream},
        client::{MAX_PACKET_SIZE, WINDOW_SIZE},
        error::*,
        executor::exec_string,
    },
};
use marathon_core::{
    error::{Failure, VendorError},
    vendor::VendorContext,
};
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
                f.kind.legacy_class().to_owned(),
                f.message.as_ref().map(|m| m.to_owned()),
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
        [(
            OPEN_FAIL_EXCEPTION.legacy_class().into(),
            Some("Resource shortage".into())
        )]
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
    assert_eq!(
        throwable.as_ref().unwrap().kind.legacy_class(),
        OPEN_FAIL_EXCEPTION.legacy_class().to_owned()
    );
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
        [(
            CONNECTION_EXCEPTION.legacy_class().into(),
            Some("Request failed".into())
        )]
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
            [(
                CONNECTION_EXCEPTION.legacy_class().into(),
                Some("Request failed".into())
            )]
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
        [(
            CONNECTION_EXCEPTION.legacy_class().into(),
            Some("Request failed".into())
        )]
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

/// A script where the command `silent` gets no reply, while all others work.
fn silent_exec_script(withhold_close: bool) -> fake_worker::script::Script {
    fake_worker::script::Script {
        faults: Faults {
            silent_exec: Some("silent".into()),
            withhold_close,
            ..Default::default()
        },
        ..ssh::suite_script()
    }
}
const NO_REPLY_LINE: &str = "No reply to exec `echo silent` in 30 s: closing its channel. If the worker gets to the request later, the command may still start, and hold its session slot until it ends";
fn assert_is_the_exec_timeout(failure: &Failure) {
    // Kotlin's expiry of the same wait, which callers already handle: `safe_execute` returns null for it.
    assert_eq!(
        class_chain(failure),
        [
            (
                CONNECTION_EXCEPTION.legacy_class().to_owned(),
                Some("Timeout expired: 30000 MILLISECONDS".to_owned())
            ),
            (
                TIMEOUT_EXCEPTION.legacy_class().to_owned(),
                Some("Timeout expired: 30000 MILLISECONDS".to_owned())
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exec_with_no_reply_fails_with_its_timeout_and_gives_its_slot_back_u154() {
    // One slot, so that a channel left open would make the next command fail to open one. 30 s in compressed time.
    let fake = ssh::fake(silent_exec_script(false), 1).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    let started = Instant::now();
    let failure = other(
        executor
            .execute(request(&["echo", "silent"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert!(started.elapsed() >= Duration::from_secs(3), "{:?}", started.elapsed());
    assert_is_the_exec_timeout(&failure);
    let lines = ssh::lines(&events);
    let logged: Vec<_> = lines.iter().filter(|(_, level, _)| *level == Level::Error).collect();
    assert_eq!(logged.len(), 1, "{lines:?}");
    assert_eq!(logged[0].2, NO_REPLY_LINE);
    assert!(executor.connected());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
    let wire: Vec<_> = fake
        .transcript()
        .await
        .into_iter()
        .filter_map(|event| {
            let name = event["event"].as_str().unwrap().to_owned();
            ["exec", "signal", "client_close"]
                .contains(&name.as_str())
                .then_some(name)
        })
        .take(2)
        .collect();
    assert_eq!(wire, ["exec", "client_close"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exec_with_no_reply_from_a_peer_that_never_answers_close_still_returns_u154() {
    let fake = ssh::fake(silent_exec_script(true), 4).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    let started = Instant::now();
    let failure = other(
        tokio::time::timeout(
            Duration::from_secs(60),
            executor.execute(request(&["echo", "silent"], 5000), context()),
        )
        .await
        .unwrap()
        .err()
        .unwrap(),
    );
    // Both waits, 30 s and 5 s, in compressed time.
    assert!(
        started.elapsed() >= Duration::from_millis(3500),
        "{:?}",
        started.elapsed()
    );
    assert_is_the_exec_timeout(&failure);
    assert_eq!(fake.events_at_least("client_close", 1).await.len(), 1);
    // Neither wait logs a `woke to` line: nothing woke them.
    assert!(
        !ssh::lines(&events).iter().any(|(logger, ..)| logger == "Promise"),
        "{:?}",
        ssh::lines(&events)
    );
    assert!(executor.connected());
}

/// `waiting` holds its request, and another session on the connection gets an answer meanwhile. Factor 1, so that the wait it
/// competes with is a real 30 s, and this does not bound the host's speed.
async fn another_session_answers_during<F: std::future::Future>(
    executor: &SshCommandExecutor,
    waiting: F,
    held_up: &ssh::Fake,
    event: &str,
) {
    tokio::pin!(waiting);
    // This one must be the first, which is the one the fault holds back.
    tokio::select! {
        _ = &mut waiting => panic!("the held request returned"),
        _ = held_up.events_at_least(event, 1) => {}
    }
    let answer = tokio::select! {
        _ = &mut waiting => panic!("the wait ended before another session got an answer"),
        answer = ssh::run(executor, &["echo", "hello"], 600_000) => answer,
    };
    assert_eq!(answer.unwrap().stdout, ["hello"]);
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn other_sessions_keep_working_while_an_exec_waits_for_a_reply_u154() {
    let fake = ssh::fake(silent_exec_script(false), 4).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    let silent = request(&["echo", "silent"], 600_000);
    another_session_answers_during(&executor, executor.execute(silent, context()), &fake, "exec").await;
}

/// A script where the first open of a connection is answered this late, in real time. These run at factor 10, so the open's
/// 30 s are 3 s here.
fn late_open_script(delay_millis: u64, reject: bool) -> fake_worker::script::Script {
    fake_worker::script::Script {
        faults: Faults {
            late_open: Some(fake_worker::script::LateOpen { delay_millis, reject }),
            ..Default::default()
        },
        ..ssh::suite_script()
    }
}
const NO_OPEN_REPLY_LINE: &str =
    "No reply to opening chan#0 in 30 s. If the worker answers later, that channel will be closed then";

#[tokio::test(flavor = "multi_thread")]
async fn an_open_with_no_reply_fails_with_its_timeout_and_a_late_reply_is_closed_u157() {
    // One slot, so that the channel the late reply opens would keep the next command from running.
    let fake = ssh::fake(late_open_script(12_000, false), 1).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    let started = Instant::now();
    let failure = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert!(started.elapsed() >= Duration::from_secs(3), "{:?}", started.elapsed());
    assert_is_the_exec_timeout(&failure);
    let lines = ssh::lines(&events);
    let logged: Vec<_> = lines.iter().filter(|(_, level, _)| *level == Level::Error).collect();
    assert_eq!(logged.len(), 1, "{lines:?}");
    assert_eq!(logged[0].2, NO_OPEN_REPLY_LINE);
    // The reply comes after the caller has gone, and the channel it opens is closed, exec-less.
    assert_eq!(fake.events_at_least("late_open_reply", 1).await.len(), 1);
    assert_eq!(fake.events_at_least("client_close", 1).await.len(), 1);
    assert!(fake.events("exec").await.is_empty());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_open_inside_its_30_s_still_works_u157() {
    let fake = ssh::fake(late_open_script(1_000, false), 1).await;
    let (executor, events) = executor_with(&fake, 10, 0).await;
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
    assert_eq!(fake.events_at_least("late_open_reply", 1).await.len(), 1);
    let lines = ssh::lines(&events);
    assert!(!lines.iter().any(|(_, level, _)| *level == Level::Error), "{lines:?}");
    assert_eq!(fake.events("exec").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_that_goes_away_during_an_open_leaves_no_channel_behind_u157() {
    let fake = ssh::fake(late_open_script(3_000, false), 1).await;
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let mut open = Box::pin(executor.connection().start_session());
    tokio::select! {
        _ = &mut open => panic!("the held open returned"),
        _ = fake.events_at_least("session_open", 1) => {}
    }
    drop(open);
    assert_eq!(fake.events_at_least("client_close", 1).await.len(), 1);
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

/// Wakes the test when the open's reply has reached the caller, without polling the caller again.
struct Woken(tokio::sync::Notify);
impl std::task::Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
}

/// An open polled once, with a waker of its own, so that a test can see its reply arrive without polling it again.
struct HeldOpen<F> {
    open: std::pin::Pin<Box<F>>,
    woken: Arc<Woken>,
    waker: std::task::Waker,
}
impl<F: Future> HeldOpen<F> {
    fn start(open: F) -> Self {
        let woken = Arc::new(Woken(tokio::sync::Notify::new()));
        let mut held = HeldOpen {
            open: Box::pin(open),
            waker: std::task::Waker::from(woken.clone()),
            woken,
        };
        assert!(held.poll().is_pending());
        held
    }
    fn poll(&mut self) -> std::task::Poll<F::Output> {
        self.open
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&self.waker))
    }
    /// At factor 1 the open's 30 s are far behind a reply that comes in 1 s or at once, so the wake is the reply.
    async fn replied(&self) {
        tokio::time::timeout(Duration::from_secs(60), self.woken.0.notified())
            .await
            .expect("the reply reaches the caller");
    }
}

/// Starts an open, waits until its reply is in the caller's oneshot, and drops the caller there, unpolled.
async fn drop_the_caller_once_its_reply_came(executor: &SshCommandExecutor, fake: &ssh::Fake) {
    let open = HeldOpen::start(executor.connection().start_session());
    assert_eq!(fake.events_at_least("late_open_reply", 1).await.len(), 1);
    open.replied().await;
    drop(open);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_that_goes_away_after_its_reply_came_leaves_no_channel_behind_u157() {
    // One slot, so that a channel left open would refuse the next command.
    let fake = ssh::fake(late_open_script(1_000, false), 1).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    drop_the_caller_once_its_reply_came(&executor, &fake).await;
    assert_eq!(fake.events_at_least("client_close", 1).await.len(), 1);
    assert!(fake.events("exec").await.is_empty());
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_caller_that_goes_away_after_its_refusal_came_leaves_no_text_behind_u157() {
    let fake = ssh::fake(late_open_script(1_000, true), 1).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    drop_the_caller_once_its_reply_came(&executor, &fake).await;
    let held = start(&executor, &["sleep", "1000"], 600_000).await;
    let refused = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert_eq!(
        class_chain(&refused),
        [(
            OPEN_FAIL_EXCEPTION.legacy_class().into(),
            Some("Resource shortage".into())
        )]
    );
    held.terminate().unwrap();
    held.close().await.unwrap();
}

/// Two refusals waiting in their callers at once (x38 review round 2): the first is dropped unclaimed and the second is read
/// at once. On one thread nothing else runs between the drop and that read, so a text taken from a queue shared by the
/// connection would be the first one's.
#[tokio::test]
async fn a_refusal_dropped_unclaimed_does_not_give_its_text_to_one_read_after_it_u157() {
    let fake = ssh::fake(late_open_script(1_000, true), 1).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    let first = HeldOpen::start(executor.connection().start_session());
    assert_eq!(fake.events_at_least("late_open_reply", 1).await.len(), 1);
    first.replied().await;
    let held = start(&executor, &["sleep", "1000"], 600_000).await;
    let mut second = HeldOpen::start(executor.connection().start_session());
    second.replied().await;
    drop(first);
    let std::task::Poll::Ready(refused) = second.poll() else {
        panic!("the second open's reply was in its oneshot")
    };
    assert_eq!(
        class_chain(&other(refused.err().unwrap().into_vendor())),
        [(
            OPEN_FAIL_EXCEPTION.legacy_class().into(),
            Some("Resource shortage".into())
        )]
    );
    held.terminate().unwrap();
    held.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_refusal_does_not_give_its_text_to_the_next_failure_u157() {
    let fake = ssh::fake(late_open_script(12_000, true), 1).await;
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let failure = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert_is_the_exec_timeout(&failure);
    assert_eq!(fake.events_at_least("late_open_reply", 1).await.len(), 1);
    // Its text reaches the client a moment after the event, and is off the queue a moment after that.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let held = start(&executor, &["sleep", "1000"], 600_000).await;
    let refused = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert_eq!(
        class_chain(&refused),
        [(
            OPEN_FAIL_EXCEPTION.legacy_class().into(),
            Some("Resource shortage".into())
        )]
    );
    held.terminate().unwrap();
    held.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_that_is_never_answered_still_returns_u157() {
    let fake = ssh::fake(late_open_script(3_600_000, false), 4).await;
    let (executor, _) = executor_with(&fake, 10, 0).await;
    let failure = other(
        tokio::time::timeout(
            Duration::from_secs(60),
            executor.execute(request(&["echo", "hello"], 600_000), context()),
        )
        .await
        .unwrap()
        .err()
        .unwrap(),
    );
    assert_is_the_exec_timeout(&failure);
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 600_000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn other_sessions_keep_working_while_an_open_waits_for_a_reply_u157() {
    let fake = ssh::fake(late_open_script(3_600_000, false), 4).await;
    let (executor, _) = executor_with(&fake, 1, 0).await;
    let held = request(&["echo", "hello"], 600_000);
    another_session_answers_during(&executor, executor.execute(held, context()), &fake, "session_open").await;
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
        CONNECTION_EXCEPTION.legacy_class().to_owned(),
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
    assert_eq!(chain[0].0, CONNECTION_EXCEPTION.legacy_class());
    assert_eq!(chain[1].0, TRANSPORT_EXCEPTION.legacy_class());
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
            CONNECTION_EXCEPTION.legacy_class().to_owned(),
            Some("Timeout expired: 30000 MILLISECONDS".to_owned()),
        ),
        (
            TIMEOUT_EXCEPTION.legacy_class().to_owned(),
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
            CONNECTION_EXCEPTION.legacy_class().into(),
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
    assert_eq!(
        later.kind.legacy_class(),
        CONNECTION_EXCEPTION.legacy_class().to_owned()
    );
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
    assert_eq!(
        throwable.as_ref().unwrap().kind.legacy_class(),
        CONNECTION_EXCEPTION.legacy_class().to_owned()
    );
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
        [(CONNECTION_EXCEPTION.legacy_class().into(), Some(message.into()))]
    );
    assert!(!executor.connected());
    assert!(ssh::lines(&events).contains(&(
        "TransportImpl".into(),
        Level::Error,
        format!("Dying because - {message}")
    )));
}

/// D54: what ends an Apple transfer when its device's connection is lost: keep-alive's loss, with its cause, and not our own
/// close.
#[tokio::test(flavor = "multi_thread")]
async fn the_connection_loss_a_transfer_waits_for_is_keep_alives_and_not_a_close_d54() {
    let fake = ssh::fake(
        fake_worker::script::Script {
            faults: Faults {
                keepalive: Keepalive::Withhold,
                ..Default::default()
            },
            ..rules(vec![])
        },
        4,
    )
    .await;
    let (executor, _) = executor_with(&fake, 1, 1000).await;
    let lost = tokio::time::timeout(Duration::from_secs(60), executor.connection_lost())
        .await
        .expect("keep-alive's loss did not reach the transfer");
    assert_eq!(
        class_chain(&lost),
        [(
            CONNECTION_EXCEPTION.legacy_class().into(),
            Some("Did not receive any keep-alive response for 2 seconds".into())
        )]
    );
    // Asked for after the loss, it is ready at once.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(60), executor.connection_lost()).await,
        Ok(lost)
    );
    let healthy = ssh::fake(ssh::suite_script(), 1).await;
    let (executor, _) = executor_with(&healthy, 1, 0).await;
    let waiting = executor.connection_lost();
    executor.close().await.unwrap();
    assert!(!executor.connected());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting).await.is_err(),
        "our own close is not a loss"
    );
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

/// The script of `fixtures/oracle/ssh/flood.jsonl`: 32 MiB of stdout with a stderr byte after each MiB.
fn stderr_flood() -> Action {
    let mut chunks = Vec::new();
    for _ in 0..32 {
        chunks.push(ssh::chunk(&"x".repeat(1024 * 1024)));
        chunks.push(ssh::err_chunk("e"));
    }
    ssh::output(chunks, 0)
}
/// Lines a consumer can stall on (`LineBuffer` holds a line until it ends), with stderr every 64 KiB.
///
/// A stderr read grants credit only once the window is down to sshj's threshold of 512 KiB, so stderr must come more
/// often than that. Any rarer, and each read finds the window still high, grants nothing, and the session blocks on
/// the initial 2 MiB, which is what happens without D37 as well. `ssh::wait_for_the_cap` checks that it didn't.
fn flood_reaching_the_cap() -> Action {
    let line = format!("{}\n", "x".repeat(1023));
    let mut chunks = Vec::new();
    for _ in 0..512 {
        chunks.push(ssh::chunk(&line.repeat(64)));
        chunks.push(ssh::err_chunk("e\n"));
    }
    ssh::output(chunks, 0)
}
/// Reads stderr only, until nothing more can arrive: the window is spent and stderr empty. Returns the bytes read.
///
/// A read grants credit only while the window is at or below sshj's adjust threshold (`Window.Local`:
/// min(32 KiB * 20, 2 MiB / 4)), so a read that comes earlier leaves the flood blocked well below the cap. Each read
/// here waits for a spent window, a timing that also overflowed sshj's buffer.
async fn read_stderr_until_blocked(channel: &Arc<ExecChannel>, executor: &SshCommandExecutor) -> usize {
    use tokio::io::AsyncReadExt;
    let mut stderr = channel.reader(Stream::Stderr);
    let mut consumed = 0;
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        while executor.connected() {
            if channel.local_win_size() > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            } else if channel.available(Stream::Stderr) > 0 {
                consumed += stderr.read(&mut [0u8; 1]).await.unwrap();
            } else {
                return;
            }
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "still flowing after 60 s: window {}, stdout {}, read {consumed}",
        channel.local_win_size(),
        channel.available(Stream::Stdout)
    );
    consumed
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_only_stderr_blocks_at_the_cap_without_losing_the_connection_d37() {
    // A32: on sshj, stderr reads refilled the shared window regardless of stdout, unread stdout reached
    // CircularBuffer's cap, and the reader thread died and took the connection. Oracle, kept as the witness:
    // fixtures/oracle/ssh/flood.jsonl (`tools/oracle/ssh-probes/run.sh flood`), this script through stderr only.
    use tokio::io::AsyncReadExt;
    const CAP: usize = 16 * 1024 * 1024;
    let oracle: serde_json::Value = {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/ssh/flood.jsonl");
        let text = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(text.lines().next().unwrap()).unwrap()
    };
    assert_eq!(oracle["case"], "flood_stderr_only");
    assert_eq!(oracle["connected"], false);
    assert_eq!(oracle["read_error"][0]["message"], "Attempted overflow");
    let fake = ssh::fake(rules(vec![ssh::exact("flood", stderr_flood())]), 4).await;
    let (logging, events) = ssh::logging(false);
    let executor = ssh::connect_with(&fake, &ssh::factory(&logging), false).await.unwrap();
    let channel = executor.connection().start_session().await.unwrap();
    channel.exec("flood".into()).await.unwrap();

    let consumed = read_stderr_until_blocked(&channel, &executor).await;
    assert!(executor.connected(), "the connection must survive");
    assert!(consumed < 32, "all 32 stderr bytes arrived: nothing was held back");
    let buffered = channel.available(Stream::Stdout);
    assert!(buffered < CAP, "{buffered} unread bytes reach the buffer cap");
    assert!(
        buffered > CAP - 64,
        "{buffered} unread: the window closed before the cap"
    );
    assert!(channel.is_open());
    assert!(!ssh::lines(&events).iter().any(|(_, _, line)| line.contains("Dying")));

    // A command on the same connection while blocked is checked on real OpenSSH only (`ssh_real_sshd.rs`): the fake's
    // russh session can hold back every channel in this state, which OpenSSH does not (U153).

    // Once its consumer resumes, nothing was lost.
    let (mut stdout, mut stderr) = (channel.reader(Stream::Stdout), channel.reader(Stream::Stderr));
    let (mut out, mut err) = (0, consumed);
    let mut bytes = vec![0; 64 * 1024];
    tokio::time::timeout(Duration::from_secs(60), async {
        while out < 32 * 1024 * 1024 || err < 32 {
            if channel.available(Stream::Stdout) > 0 {
                out += stdout.read(&mut bytes).await.unwrap();
            } else if channel.available(Stream::Stderr) > 0 {
                err += stderr.read(&mut bytes).await.unwrap();
            } else {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((out, err), (32 * 1024 * 1024, 32));
    channel.close_escalating().await.unwrap();
    assert_eq!(channel.exit_status(), Some(0));
    assert!(executor.connected());
    executor.close().await.unwrap();
}

/// A session whose stdout consumer has stopped for good while its stderr consumer keeps going, as a stuck callback
/// does. Nothing ever notifies `released`.
fn consume_with_stuck_stdout(
    session: Arc<CommandSession>,
    released: Arc<tokio::sync::Notify>,
) -> tokio::task::JoinHandle<Result<Option<i32>, VendorError>> {
    tokio::spawn(async move {
        consume_output(
            &session.base,
            &context(),
            move |_| {
                let released = released.clone();
                async move {
                    released.notified().await;
                    Ok(())
                }
            },
            |_| async { Ok(()) },
        )
        .await
    })
}

/// How a close at the cap may end on the fake: at once, or by the client's 30 s bound expiring (D30). Its russh session
/// can hold bytes past the window, and then defers the exit status and the CLOSE reply until they drain, which here is
/// never (U153). OpenSSH answers at once, which `ssh_real_sshd.rs` asserts. Nothing else is accepted. This can't tell a
/// close that worked from one that never got a reply: the strict outcomes are only in those opt-in tests.
fn assert_closed_or_close_wait_expired(result: Result<(), VendorError>) {
    if let Err(error) = result {
        assert_eq!(
            class_chain(&other(error)),
            [
                (
                    CONNECTION_EXCEPTION.legacy_class().into(),
                    Some("Timeout expired: 30000 MILLISECONDS".into())
                ),
                (
                    TIMEOUT_EXCEPTION.legacy_class().into(),
                    Some("Timeout expired: 30000 MILLISECONDS".into())
                ),
            ]
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_consumer_at_the_cap_is_ended_by_the_command_timeout_d37() {
    // What guarantees progress with no idle limit: TERM and CLOSE need no window, so the command timeout still works
    // on a worker blocked behind a closed window.
    let fake = ssh::fake(rules(vec![ssh::exact("flood", flood_reaching_the_cap())]), 4).await;
    let executor = ssh::connect(&fake).await;
    let expire = Arc::new(tokio::sync::Notify::new());
    let gated = VendorContext {
        clock: Arc::new(ssh::GatedTimeoutClock::new(expire.clone())),
        ..context()
    };
    let session = Arc::new(executor.execute(request(&["flood"], 600_000), gated).await.unwrap());
    let consumer = consume_with_stuck_stdout(session.clone(), Arc::new(tokio::sync::Notify::new()));
    ssh::wait_for_the_cap(&executor).await;
    expire.notify_one();
    let exit = tokio::time::timeout(Duration::from_secs(60), session.base.exit_code.wait())
        .await
        .unwrap();
    assert_closed_or_close_wait_expired(exit.map(|code| assert_eq!(code, None, "expired: no exit status")));
    assert_eq!(fake.events_at_least("signal", 1).await[0]["detail"]["signal"], "TERM");
    assert!(executor.connected());
    consumer.abort();
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_consumer_at_the_cap_is_ended_by_the_idle_timeout_d37() {
    // The other stream's reader is not blocked. It sees no data once the window is closed, so its idle check fires.
    // Each stderr read restarts the idle clock, so the limit only has to outlast a gap between two of them.
    let fake = ssh::fake(rules(vec![ssh::exact("flood", flood_reaching_the_cap())]), 4).await;
    let executor = ssh::connect(&fake).await;
    let mut flood = request(&["flood"], 600_000);
    flood.idle_timeout = duration(5000);
    let session = Arc::new(executor.execute(flood, context()).await.unwrap());
    let consumer = consume_with_stuck_stdout(session.clone(), Arc::new(tokio::sync::Notify::new()));
    ssh::wait_for_the_cap(&executor).await;
    let error = tokio::time::timeout(Duration::from_secs(60), consumer)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.failure().kind.legacy_class().to_owned(),
        "java.util.concurrent.TimeoutException"
    );
    assert!(executor.connected());
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_blocked_at_the_cap_can_be_closed_d37() {
    let fake = ssh::fake(rules(vec![ssh::exact("flood", flood_reaching_the_cap())]), 4).await;
    let executor = ssh::connect(&fake).await;
    let session = Arc::new(start(&executor, &["flood"], 600_000).await);
    let consumer = consume_with_stuck_stdout(session.clone(), Arc::new(tokio::sync::Notify::new()));
    ssh::wait_for_the_cap(&executor).await;
    let closed = tokio::time::timeout(Duration::from_secs(60), session.close())
        .await
        .unwrap();
    assert_closed_or_close_wait_expired(closed);
    assert_eq!(fake.events_at_least("signal", 1).await[0]["detail"]["signal"], "TERM");
    assert!(executor.connected());
    consumer.abort();
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_at_the_cap_does_not_turn_a_close_into_another_error_d37() {
    // A process that ignores TERM, at the cap: D30's KILL goes out with the window closed, and the close either
    // succeeds or ends in the bound the fake can force (U153). Factor 5 keeps D30's 20 s from dominating the suite.
    let line = format!("{}\n", "x".repeat(1023));
    let mut chunks = Vec::new();
    for _ in 0..512 {
        chunks.push(ssh::chunk(&line.repeat(64)));
        chunks.push(ssh::err_chunk("e\n"));
    }
    let fake = ssh::fake(rules(vec![ssh::exact("flood", hang_with(chunks, &["TERM"], false))]), 4).await;
    let (executor, _) = executor_with(&fake, 5, 0).await;
    let session = Arc::new(start(&executor, &["flood"], 600_000).await);
    let consumer = consume_with_stuck_stdout(session.clone(), Arc::new(tokio::sync::Notify::new()));
    ssh::wait_for_the_cap(&executor).await;
    let closed = tokio::time::timeout(Duration::from_secs(60), session.close())
        .await
        .unwrap();
    assert_closed_or_close_wait_expired(closed);
    let signals: Vec<_> = fake
        .events_at_least("signal", 2)
        .await
        .iter()
        .map(|e| e["detail"]["signal"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(signals, ["TERM", "KILL"]);
    assert!(executor.connected());
    consumer.abort();
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stderr_in_small_packets_reaches_the_cap_without_losing_the_connection_d37() {
    // D32's `env NSUnbufferedIO=YES`: xcodebuild writes as it goes, so stderr comes in many small packets, each read
    // of which can grant credit. That reaches the cap sooner, and it must stop there all the same.
    let line = format!("{}\n", "x".repeat(1023));
    let mut chunks = Vec::new();
    // Every 16 KiB: each packet costs the fake a transcript line, and finer than this only makes the test slow.
    for _ in 0..2048 {
        chunks.push(ssh::chunk(&line.repeat(16)));
        chunks.push(ssh::err_chunk("e\n"));
    }
    let fake = ssh::fake(rules(vec![ssh::exact("flood", ssh::output(chunks, 0))]), 4).await;
    let executor = ssh::connect(&fake).await;
    let session = Arc::new(start(&executor, &["flood"], 600_000).await);
    let consumer = consume_with_stuck_stdout(session.clone(), Arc::new(tokio::sync::Notify::new()));
    ssh::wait_for_the_cap(&executor).await;
    consumer.abort();
    executor.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_after_the_close_only_queues_an_adjust_that_russh_skips_d37() {
    // D30's CLOSE makes russh forget the channel, and D19 still drains what arrived. A read then grants credit as ever:
    // the adjust must reach nothing, least of all the connection. The initial window only, so no grant ran before (U153).
    use tokio::io::AsyncReadExt;
    let flood = ssh::output(vec![ssh::chunk(&"x".repeat(8 * 1024 * 1024))], 0);
    let fake = ssh::fake(rules(vec![ssh::exact("flood", flood)]), 4).await;
    let executor = ssh::connect(&fake).await;
    let channel = executor.connection().start_session().await.unwrap();
    channel.exec("flood".into()).await.unwrap();
    wait_for_channel_state(&channel, "initial window did not drain", || {
        !executor.connected() || channel.local_win_size() == 0
    })
    .await;
    channel.signal(russh::Sig::TERM).unwrap();
    channel.close_escalating().await.unwrap();
    let window = channel.local_win_size();
    let mut bytes = vec![0; 64 * 1024];
    let read = channel.reader(Stream::Stdout).read(&mut bytes).await.unwrap();
    assert_eq!(read, bytes.len());
    assert!(channel.local_win_size() > window, "the read granted credit");
    assert_eq!(
        ssh::run(&executor, &["echo", "hello"], 60_000).await.unwrap().stdout,
        ["hello"]
    );
    assert!(executor.connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transport_that_dies_during_the_close_is_a_connection_failure_d36() {
    // The transport dies during D30's TERM wait, which then ends, so no KILL is attempted. The close ends at once with
    // the connection failure, and the next command finds the connection gone: nothing here is a state error.
    let hanging = Action::Output {
        chunks: vec![],
        status: 0,
        signal: None,
        hang: true,
        ignore_signals: vec!["TERM".into()],
        eof_before_exit: false,
        close_before_eof: false,
        withhold_close: false,
    };
    let fake = ssh::fake(rules(vec![ssh::exact("stubborn", hanging)]), 4).await;
    let executor = ssh::connect(&fake).await;
    let session = start(&executor, &["stubborn"], 600_000).await;
    let (closed, ()) = tokio::join!(session.close(), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        executor.connection().shared.die(SshFailure::transport(
            DisconnectReason::ConnectionLost,
            "fixture transport loss",
        ));
    });
    assert_eq!(
        class_chain(&other(closed.unwrap_err())),
        [
            (
                CONNECTION_EXCEPTION.legacy_class().into(),
                Some("fixture transport loss".into())
            ),
            (
                TRANSPORT_EXCEPTION.legacy_class().into(),
                Some("fixture transport loss".into())
            ),
        ]
    );
    assert!(!executor.connected());
    assert!(executor.connection().shared.is_dead());
    assert_eq!(
        executor
            .safe_execute(request(&["echo", "hello"], 5000), context())
            .await,
        Ok(None)
    );
    let error = other(
        executor
            .execute(request(&["echo", "hello"], 5000), context())
            .await
            .err()
            .unwrap(),
    );
    assert_eq!(
        error.kind.legacy_class().to_owned(),
        CONNECTION_EXCEPTION.legacy_class()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn both_streams_read_promptly_deliver_everything_d37() {
    // The ordinary case on the wire; `channel::tests` shows that its grants are exactly sshj's.
    let fake = ssh::fake(rules(vec![ssh::exact("flood", stderr_flood())]), 4).await;
    let executor = ssh::connect(&fake).await;
    let result = ssh::run(&executor, &["flood"], 60_000).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.concat().len(), 32 * 1024 * 1024);
    assert_eq!(result.stderr.concat().len(), 32);
    assert!(executor.connected());
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
            (CONNECTION_EXCEPTION.legacy_class().into(), Some("Disconnected".into())),
            (TRANSPORT_EXCEPTION.legacy_class().into(), Some("Disconnected".into())),
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
