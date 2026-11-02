//! The T01-T19 cases, printing the same keys as spikes/ssh-sshj/src/Harness.kt

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::exec::{self, CommandResult, Executor, Target, kotlin_list};
use crate::quote::{shell_command, write_file_command};

pub fn out(target: &Target, key: &str, value: impl std::fmt::Display) {
    println!("{key}: {}", value.to_string().replace(&target.host, "<host>"));
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn shell(command: &str) {
    let _ = Command::new("sh").arg("-c").arg(command).status();
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn describe(e: &anyhow::Error) -> String {
    e.chain().map(|c| format!("{c}")).collect::<Vec<_>>().join(" <- ")
}

const S: fn(u64) -> Duration = Duration::from_secs;

/// sshj's TERM-or-null exit codes print as null in the Kotlin harness
fn show(result: &anyhow::Result<CommandResult>) -> String {
    match result {
        Ok(r) => r.show(),
        Err(e) => format!("error {}", describe(e)),
    }
}

/// Transient per the Kotlin isTransientConnectionFailure, mapped onto russh's errors: network failures, timeouts and
/// dropped transports and no-common-algorithm failures are transient; auth, host keys and unreadable keys are not.
pub fn is_transient(e: &anyhow::Error) -> bool {
    // A generic russh Disconnect wrapper must not hide the explicit wire reason beneath it.
    if let Some(exec::PeerDisconnected(reason, _)) = e.chain().find_map(|c| c.downcast_ref::<exec::PeerDisconnected>())
    {
        return matches!(reason, 10 | 12 | 11 | 7 | 5);
    }
    for cause in e.chain() {
        if cause.downcast_ref::<exec::AuthFailed>().is_some() {
            return false;
        }
        // Not an IOException in Kotlin, so ConnectionFactory never wraps it into UnreachableHost
        if matches!(
            cause.downcast_ref::<exec::ExecError>(),
            Some(exec::ExecError::NotConnected)
        ) {
            return false;
        }
        if let Some(err) = cause.downcast_ref::<russh::Error>() {
            use russh::Error::*;
            // sshj reports a failed key exchange as TransportException[UNKNOWN], and UNKNOWN is in TRANSIENT_DISCONNECTS,
            // so it is transient in practice although the KDoc says otherwise (known bug A16, T26)
            return matches!(
                err,
                IO(_)
                    | Disconnect
                    | HUP
                    | ConnectionTimeout
                    | KeepaliveTimeout
                    | InactivityTimeout
                    | SendError
                    | NoCommonAlgo { .. }
                    | Kex
                    | KexInit
            );
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return true;
        }
    }
    false
}

pub async fn connect_case(target: &Target) {
    let attempts = AtomicU32::new(0);
    let start = Instant::now();
    // As in the Kotlin harness, a first command that fails makes the whole case an error
    let connected = match exec::connect(target, &attempts).await {
        Ok(executor) => match executor.run(&["echo", "connected"], S(60), None).await {
            Ok(r) => Ok((executor, r)),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match connected {
        Ok((executor, r)) => {
            out(target, "connect.result", format!("ok {}", kotlin_list(&r.stdout)));
            out(target, "connect.host_id", &executor.host_id);
            executor.close().await;
        }
        Err(e) => {
            out(target, "connect.result", "error");
            out(target, "connect.info.error", describe(&e));
            out(target, "connect.info.error_debug", format!("{:?}", e.root_cause()));
            out(target, "connect.transient", is_transient(&e));
        }
    }
    out(target, "connect.attempts", attempts.load(Ordering::SeqCst));
    out(target, "connect.info.elapsed_ms", start.elapsed().as_millis());
}

pub async fn t01(t: &Target, ex: &Executor) {
    out(
        t,
        "T01.stdout",
        show(&ex.run(&["echo", "Hello world"], S(5), Some(S(1))).await),
    );
    out(
        t,
        "T01.stderr",
        show(
            &ex.run(&["bash", "-c", "1>/dev/stderr echo Hello world"], S(5), Some(S(1)))
                .await,
        ),
    );
    out(
        t,
        "T01.exit",
        show(&ex.run(&["sh", "-c", "exit 1"], S(5), Some(S(1))).await),
    );
    out(t, "T01.host_id", &ex.host_id);
    out(t, "T01.charset", "UTF-8");
}

pub async fn t02(t: &Target, ex: &Executor) {
    let expected: Vec<String> = (1..=100000).map(|i| i.to_string()).collect();
    let seq = ex.run(&["sh", "-c", "seq 1 100000"], S(30), Some(S(5))).await;
    if let Ok(r) = &seq {
        out(
            t,
            "T02.seq",
            format!(
                "exit={} stdout_lines={} ordered={} stderr_lines={}",
                r.exit_code.map_or("null".into(), |c| c.to_string()),
                r.stdout.len(),
                r.stdout == expected,
                r.stderr.len()
            ),
        );
    } else {
        out(t, "T02.seq", show(&seq));
    }

    // consumeOutput: both streams concurrently
    let command = strings(&["sh", "-c", "echo start; seq 1 100000 1>&2; echo end"]);
    let mut session = ex.execute(&command, S(30), Some(S(5))).await.expect("execute");
    let r = session.await_result().await;
    match r {
        Ok(r) => out(
            t,
            "T02.concurrent",
            format!(
                "exit={} stdout={} stderr_lines={} ordered={}",
                r.exit_code.map_or("null".into(), |c| c.to_string()),
                kotlin_list(&r.stdout),
                r.stderr.len(),
                r.stderr == expected
            ),
        ),
        Err(e) => out(t, "T02.concurrent", format!("error {e}")),
    }
    let _ = session.close().await;

    // A consumer that fails on the first stdout line: both streams are dropped, then close() must not wedge
    let command = strings(&["sh", "-c", "echo start; exec seq 1 100000 1>&2"]);
    let mut session = ex.execute(&command, S(60), Some(S(30))).await.expect("execute");
    let mut stdout = session.stdout.take().expect("stdout");
    let stderr = session.stderr.take();
    let error = match stdout.recv().await {
        Some(Ok(line)) => format!("consumer failed on {line}"),
        other => format!("{other:?}"),
    };
    drop((stdout, stderr));
    let (outcome, ms) = timed_close(&mut session, CLOSE_CAP).await;
    out(t, "T02.consumer_failure", format!("error={error}"));
    out(t, "T02.close_outcome", outcome);
    out(t, "T02.info.close_ms", ms);
}

pub const T03_ARGS: &[&str] = &[
    "plain",
    "a b",
    "$HOME",
    "it's",
    "'already quoted'",
    "\"double quoted\"",
    "FOO=bar",
    "/p-a.t,h:+@x",
    "'half",
    "half'",
    "'mixed\"",
    "a\"b",
    "~",
    "semi;colon",
    "",
    "tab\tx",
    "back\\slash",
    "star*",
    "$(echo injected)",
];

pub async fn t03(t: &Target, ex: &Executor) {
    for (index, arg) in T03_ARGS.iter().enumerate() {
        let command = ["printf", "<%s>\\n", arg];
        out(t, &format!("T03.{index}.wire"), shell_command(&strings(&command)));
        out(
            t,
            &format!("T03.{index}.result"),
            show(&ex.run(&command, S(10), None).await),
        );
    }
}

pub async fn t04(t: &Target, ex: &Executor) {
    // env {F=1} and workdir /tmp are accepted by the Kotlin signature and never sent; this executor has no parameter for them
    let r = show(&ex.run(&["sh", "-c", "echo F=${F-unset}; pwd"], S(10), None).await);
    let home = regex_home(&r);
    out(t, "T04.result", home);
}

fn regex_home(s: &str) -> String {
    let mut result = s.to_string();
    for prefix in ["/Users/", "/config"] {
        if let Some(start) = result.find(prefix) {
            let end = result[start..].find(']').map_or(result.len(), |e| start + e);
            result.replace_range(start..end, "<home>");
        }
    }
    result
}

pub async fn t05(t: &Target, ex: &Executor) {
    let start = Instant::now();
    out(
        t,
        "T05.result",
        show(&ex.run(&["sleep", "1717"], Duration::from_millis(10), None).await),
    );
    out(t, "T05.info.elapsed_ms", start.elapsed().as_millis());
    tokio::time::sleep(S(1)).await;
    let survivor = ex
        .run(&["pgrep", "-f", "[s]leep 1717"], S(60), None)
        .await
        .map(|r| r.exit_code == Some(0))
        .unwrap_or(false);
    out(t, "T05.remote_sleep_survives", survivor);
    if survivor {
        let _ = ex.run(&["pkill", "-f", "[s]leep 1717"], S(60), None).await;
    }
}

pub async fn t06(t: &Target, ex: &Executor) {
    let command = strings(&["sh", "-c", "while [ true ]; do date && sleep 0.1; done"]);
    // The streams are drained by a task while this one sends TERM, as the Kotlin test awaits in another coroutine
    let mut busy = ex.execute(&command, S(5), Some(S(1))).await.expect("execute");
    let stdout = busy.stdout.take();
    let stderr = busy.stderr.take();
    let collect = tokio::spawn(collect_lines(stdout, stderr));
    tokio::time::sleep(Duration::from_millis(500)).await;
    busy.terminate().await;
    let (out_lines, err_lines) = collect.await.unwrap_or_default();
    let exit = busy.await_result().await.map(|r| r.exit_code);
    out(
        t,
        "T06.busy",
        format!(
            "exit={} stdout_in_3_10={} stderr={}",
            fmt_exit(&exit),
            (3..=10).contains(&out_lines.len()),
            kotlin_list(&err_lines)
        ),
    );
    out(t, "T06.info.busy_lines", out_lines.len());

    let mut sleeper = ex
        .execute(&strings(&["sleep", "1000"]), S(600), Some(S(600)))
        .await
        .expect("execute");
    let collect = tokio::spawn(collect_lines(sleeper.stdout.take(), sleeper.stderr.take()));
    tokio::time::sleep(S(1)).await;
    sleeper.terminate().await;
    let (o, e) = collect.await.unwrap_or_default();
    let exit = sleeper.await_result().await.map(|r| r.exit_code);
    out(
        t,
        "T06.sleep",
        format!(
            "exit={} stdout={} stderr={}",
            fmt_exit(&exit),
            kotlin_list(&o),
            kotlin_list(&e)
        ),
    );
    out(t, "T06.after", show(&ex.run(&["echo", "hello"], S(60), None).await));
}

fn fmt_exit(exit: &Result<Option<i32>, exec::ExecError>) -> String {
    match exit {
        Ok(Some(code)) => code.to_string(),
        Ok(None) => "null".into(),
        Err(e) => format!("error {e}"),
    }
}

type Lines = tokio::sync::mpsc::Receiver<Result<String, exec::ExecError>>;

async fn collect_lines(stdout: Option<Lines>, stderr: Option<Lines>) -> (Vec<String>, Vec<String>) {
    async fn one(rx: Option<Lines>) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(mut rx) = rx {
            while let Some(Ok(line)) = rx.recv().await {
                lines.push(line);
            }
        }
        lines
    }
    tokio::join!(one(stdout), one(stderr))
}

pub async fn t07(t: &Target, ex: &Executor) {
    out(
        t,
        "T07.background",
        show(&ex.run(&["sleep", "100000", "&"], S(600), None).await),
    );
    out(
        t,
        "T07.background_after",
        show(&ex.run(&["echo", "hello"], S(60), None).await),
    );

    let ignoring = strings(&["sh", "-c", "trap \"\" TERM; sleep 5; echo survived"]);
    let mut session = ex.execute(&ignoring, S(30), None).await.expect("execute");
    let collect = tokio::spawn(collect_lines(session.stdout.take(), session.stderr.take()));
    tokio::time::sleep(S(1)).await;
    let start = Instant::now();
    session.terminate().await;
    let (o, e) = collect.await.unwrap_or_default();
    let exit = session.await_result().await.map(|r| r.exit_code);
    out(
        t,
        "T07.ignored_term",
        format!(
            "exit={} stdout={} stderr={}",
            fmt_exit(&exit),
            kotlin_list(&o),
            kotlin_list(&e)
        ),
    );
    out(t, "T07.info.ignored_term_ms", start.elapsed().as_millis());

    let mut closing = ex.execute(&ignoring, S(30), None).await.expect("execute");
    let drained = closing.start_await();
    tokio::time::sleep(S(1)).await;
    let (outcome, ms) = timed_close(&mut closing, CLOSE_CAP).await;
    out(t, "T07.close_outcome", outcome);
    out(t, "T07.info.close_ms", ms);
    observe_await(t, "T07.close_await", drained).await;
    out(
        t,
        "T07.close_after",
        show(&ex.run(&["echo", "hello"], S(60), None).await),
    );
    tokio::time::sleep(S(5)).await;
}

pub async fn t08(t: &Target, ex: &Executor) {
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(
            ex.execute(&strings(&["sleep", "20"]), S(60), None)
                .await
                .expect("execute"),
        );
    }
    match ex.execute(&strings(&["echo", "third"]), S(10), None).await {
        Ok(mut third) => out(
            t,
            "T08.third",
            format!(
                "opened {}",
                third.await_result().await.map(|r| r.show()).unwrap_or_default()
            ),
        ),
        Err(e) => {
            out(t, "T08.third", "error is_connection_exception=true");
            out(
                t,
                "T08.info.third_error",
                format!("{} / {:?}", describe(&e), e.root_cause()),
            );
        }
    }
    out(
        t,
        "T08.safe_execute",
        match ex.safe_execute(&strings(&["echo", "safe"]), S(10)).await {
            Some(r) => format!("CommandResult({})", r.show()),
            None => "null".into(),
        },
    );
    out(t, "T08.connected", ex.connected());
    for session in &held {
        session.terminate().await;
    }
    for mut session in held {
        out(
            t,
            "T08.held",
            session
                .await_result()
                .await
                .map(|r| r.show())
                .unwrap_or_else(|e| format!("error {e}")),
        );
    }
    out(t, "T08.after", show(&ex.run(&["echo", "after"], S(60), None).await));
}

pub async fn t09(t: &Target, ex: &Executor, wait_cap_ms: u64) {
    out(
        t,
        "T09.effective_interval_s",
        exec::keep_alive_interval(t.keep_alive).map_or(0, |d| d.as_secs()),
    );
    out(
        t,
        "T09.keep_alive_threads",
        "n/a (russh runs keep-alive inside the session task)",
    );
    let mut session = ex
        .execute(&strings(&["sleep", "25"]), S(120), None)
        .await
        .expect("execute");
    let mut result = tokio::spawn(async move { session.await_result().await });
    tokio::time::sleep(S(2)).await;
    shell(&env("SPIKE_PAUSE_CMD"));
    let paused = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_millis(wait_cap_ms), &mut result).await;
    let detect = paused.elapsed().as_millis();
    shell(&env("SPIKE_RESUME_CMD"));
    match outcome {
        Err(_) => {
            out(
                t,
                "T09.result",
                format!("no loss detected within {} s", wait_cap_ms / 1000),
            );
            let after = match result.await {
                Ok(Ok(r)) => r.show(),
                Ok(Err(e)) => format!("error {e}"),
                Err(e) => format!("error {e}"),
            };
            out(t, "T09.after_resume", after);
        }
        Ok(joined) => {
            let r = joined.expect("join");
            out(
                t,
                "T09.result",
                match &r {
                    Ok(r) => r.show(),
                    Err(exec::ExecError::ConnectionLost(_)) => "error connection lost".to_string(),
                    Err(e) => format!("error {e}"),
                },
            );
            out(
                t,
                "T09.info.error",
                r.err().map(|e| e.to_string()).unwrap_or_else(|| "null".into()),
            );
            out(t, "T09.info.detect_ms", detect);
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    out(t, "T09.connected", ex.connected());
}

pub async fn t11(t: &Target, ex: &Executor) {
    out(t, "T11.keep_alive_threads", "n/a");
    let start = Instant::now();
    out(t, "T11.idle_60s", show(&ex.run(&["sleep", "60"], S(120), None).await));
    out(t, "T11.info.elapsed_ms", start.elapsed().as_millis());
    out(t, "T11.connected", ex.connected());
}

pub async fn t14(t: &Target, ex: &Executor) {
    out(
        t,
        "T14.timeout_partial",
        show(&ex.run(&["sh", "-c", "printf partial; sleep 3"], S(1), None).await),
    );
    out(
        t,
        "T14.partial_line",
        show(&ex.run(&["printf", "a\\nb\\npartial"], S(60), None).await),
    );
    out(
        t,
        "T14.line_endings",
        show(&ex.run(&["printf", "x\\r\\ny\\rz\\n\\n\\r\\nw"], S(60), None).await),
    );
    match ex.run(&["sh", "-c", "seq 1 300000; exit 3"], S(60), None).await {
        Ok(r) => out(
            t,
            "T14.fast_exit_big",
            format!(
                "exit={} lines={} last={}",
                r.exit_code.map_or("null".into(), |c| c.to_string()),
                r.stdout.len(),
                r.stdout.last().cloned().unwrap_or_else(|| "null".into())
            ),
        ),
        Err(e) => out(t, "T14.fast_exit_big", describe(&e)),
    }
    match ex
        .run(&["sh", "-c", "seq 1 50000; seq 1 50000 1>&2"], S(60), None)
        .await
    {
        Ok(r) => out(
            t,
            "T14.both_streams_exit",
            format!(
                "exit={} stdout_lines={} stderr_lines={}",
                r.exit_code.map_or("null".into(), |c| c.to_string()),
                r.stdout.len(),
                r.stderr.len()
            ),
        ),
        Err(e) => out(t, "T14.both_streams_exit", describe(&e)),
    }

    let mut flood = ex
        .execute(&strings(&["seq", "1", "100000000"]), S(60), None)
        .await
        .expect("execute");
    let drained = flood.start_await();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (outcome, ms) = timed_close(&mut flood, CLOSE_CAP).await;
    out(t, "T14.flood_close_outcome", outcome);
    out(t, "T14.info.flood_close_ms", ms);
    observe_await(t, "T14.flood_await", drained).await;
    out(t, "T14.after", show(&ex.run(&["echo", "hello"], S(60), None).await));
    let idle = ex
        .run(
            &["sh", "-c", "echo a; sleep 3; echo b"],
            S(10),
            Some(Duration::from_millis(1000)),
        )
        .await;
    match idle {
        Ok(r) => out(t, "T14.idle_timeout", r.show()),
        Err(e) => {
            out(
                t,
                "T14.idle_timeout",
                format!("failed: {}", crate::cases_round2::kind(&e)),
            );
            out(t, "T14.info.idle_timeout_error", describe(&e));
        }
    }
}

/// The comparable outcome of `close()` (the same words as the sshj harness), and how long it took. The matrix stops
/// watching after `cap`
async fn timed_close(session: &mut exec::CommandSession, cap: Duration) -> (String, u128) {
    let start = Instant::now();
    let outcome = match tokio::time::timeout(cap, session.close()).await {
        Ok(Ok(())) => "returned".to_string(),
        Ok(Err(e)) => format!("failed: {}", crate::cases_round2::exec_kind(&e)),
        Err(_) => format!("still waiting after {} s", cap.as_secs()),
    };
    (outcome, start.elapsed().as_millis())
}

/// The comparable outcome of awaiting a session's result
async fn timed_await(session: &mut exec::CommandSession, cap: Duration) -> (String, u128) {
    let start = Instant::now();
    let outcome = match tokio::time::timeout(cap, session.await_result()).await {
        Ok(Ok(r)) => r.show(),
        Ok(Err(e)) => format!("failed: {}", crate::cases_round2::exec_kind(&e)),
        Err(_) => format!("still waiting after {} s", cap.as_secs()),
    };
    (outcome, start.elapsed().as_millis())
}

const CLOSE_CAP: Duration = Duration::from_secs(45);

/// T15: close() of an open session against `serve --hang-on-close`
pub async fn t15(t: &Target, ex: &Executor) {
    let mut session = ex
        .execute(&strings(&["sleep", "30"]), S(60), None)
        .await
        .expect("execute");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (outcome, ms) = timed_close(&mut session, S(90)).await;
    out(t, "T15.close", outcome);
    out(t, "T15.info.close_ms", ms);
}

/// T15h: the command timeout fires against `serve --hang-on-close`; then the caller closes the session
pub async fn t15h(t: &Target, ex: &Executor) {
    let mut session = ex
        .execute(&strings(&["sleep", "30"]), S(1), None)
        .await
        .expect("execute");
    let (outcome, ms) = timed_await(&mut session, S(90)).await;
    out(t, "T15h.await", outcome);
    out(t, "T15h.info.await_ms", ms);
    let (outcome, ms) = timed_close(&mut session, S(60)).await;
    out(t, "T15h.close_after", outcome);
    out(t, "T15h.info.close_after_ms", ms);
}

/// T15t: the command timeout fires on a process that ignores TERM (real sshd)
pub async fn t15t(t: &Target, ex: &Executor) {
    let command = strings(&["sh", "-c", "trap \"\" TERM; sleep 8; echo survived"]);
    let mut session = ex.execute(&command, S(1), None).await.expect("execute");
    let (outcome, ms) = timed_await(&mut session, S(90)).await;
    out(t, "T15t.await", outcome);
    out(t, "T15t.info.await_ms", ms);
    let (outcome, _) = timed_close(&mut session, S(60)).await;
    out(t, "T15t.close_after", outcome);
    out(t, "T15t.after", show(&ex.run(&["echo", "hello"], S(60), None).await));
}

pub async fn t16(t: &Target, first: Executor) {
    let mut first = Some(first);
    for signal in ["KILL", "SEGV", "TERM", "INT", "HUP", "USR2"] {
        let ex = match first.take() {
            Some(ex) => ex,
            None => exec::connect(t, &AtomicU32::new(0)).await.expect("connect"),
        };
        let command = format!("kill -{signal} $$");
        match ex.run(&["sh", "-c", &command], S(10), None).await {
            Ok(r) => out(
                t,
                &format!("T16.{signal}"),
                format!(
                    "exit={} raw_status={} raw_signal={}",
                    r.exit_code.map_or("null".into(), |c| c.to_string()),
                    r.raw.status.map_or("null".into(), |c| c.to_string()),
                    r.raw.signal.clone().unwrap_or_else(|| "null".into())
                ),
            ),
            Err(e) => out(t, &format!("T16.{signal}"), describe(&e)),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let survives = ex
            .run(&["echo", "hello"], S(10), None)
            .await
            .map(|r| r.stdout == ["hello"])
            .unwrap_or(false);
        out(t, &format!("T16.{signal}.connection_survives"), survives);
        ex.close().await;
    }
}

pub async fn t18(t: &Target, ex: &Executor) {
    async fn write_file(ex: &Executor, content: &[u8], dst: &str) -> bool {
        match write_file_command(content, dst) {
            None => false,
            Some(command) => match ex.execute(&command, S(30), None).await {
                Ok(mut session) => session
                    .await_result()
                    .await
                    .map(|r| r.exit_code == Some(0))
                    .unwrap_or(false),
                Err(_) => false,
            },
        }
    }
    out(
        t,
        "T18.small",
        write_file(ex, b"hello\n", "/tmp/spike-write-small").await,
    );
    out(
        t,
        "T18.small_wire",
        shell_command(&write_file_command(b"hello\n", "/tmp/spike-write-small").unwrap_or_default()),
    );
    let content: Vec<u8> = (0..32 * 1024).map(|i: usize| (i * 31 % 251) as u8).collect();
    out(t, "T18.max", write_file(ex, &content, "/tmp/spike-write-max").await);
    let remote = ex
        .run(&["md5sum", "/tmp/spike-write-max"], S(60), None)
        .await
        .map(|r| r.stdout.join(""));
    let tmp = std::env::temp_dir().join("spike-write-max");
    let _ = std::fs::write(&tmp, &content);
    let local = Command::new("md5")
        .arg("-q")
        .arg(&tmp)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let matches = match (remote, local) {
        (Ok(remote), Ok(local)) => remote.split(' ').next() == Some(local.as_str()),
        _ => false,
    };
    out(t, "T18.max_md5_matches", matches);
    out(
        t,
        "T18.oversize",
        write_file(ex, &vec![0u8; 32 * 1024 + 1], "/tmp/spike-write-big").await,
    );
    out(t, "T18.bad_dst", write_file(ex, b"x", "/tmp/a b").await);
    out(
        t,
        "T18.failing_dst",
        write_file(ex, b"x", "/nonexistent-dir/file").await,
    );
    let _ = ex
        .run(
            &["rm", "-f", "/tmp/spike-write-small", "/tmp/spike-write-max"],
            S(60),
            None,
        )
        .await;
}

async fn observe_await(
    t: &Target,
    key: &str,
    pending: tokio::task::JoinHandle<Result<CommandResult, exec::ExecError>>,
) {
    let result = pending.await;
    let outcome = match &result {
        Ok(Ok(_)) => "returned",
        Ok(Err(exec::ExecError::Cancelled)) => "cancelled",
        _ => "failed",
    };
    out(t, key, outcome);
    let detail = match result {
        Ok(Ok(r)) => format!(
            "exit={:?} stdout_lines={} stderr_lines={}",
            r.exit_code,
            r.stdout.len(),
            r.stderr.len()
        ),
        other => format!("{other:?}"),
    };
    out(t, &format!("{key}.info.result"), detail);
}
