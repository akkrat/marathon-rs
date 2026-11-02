//! Review round 1 cases (T21-T25), printing the same keys as spikes/ssh-sshj/src/Round2.kt

use std::time::{Duration, Instant};

use crate::cases::out;
use crate::exec::{CommandResult, ExecError, Executor, Target};

const S: fn(u64) -> Duration = Duration::from_secs;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The error kinds both harnesses print, so the comparison does not depend on class names
pub fn kind(e: &anyhow::Error) -> String {
    match e.downcast_ref::<ExecError>() {
        Some(error) => exec_kind(error),
        None => "other".into(),
    }
}

pub fn exec_kind(e: &ExecError) -> String {
    match Some(e) {
        Some(ExecError::ConnectionLost(_)) => "connection lost".into(),
        Some(ExecError::IdleTimeout(_)) => "idle timeout".into(),
        Some(ExecError::CloseTimeout(_)) => "close timeout".into(),
        Some(ExecError::ExecRejected(_)) => "exec rejected".into(),
        Some(ExecError::Protocol(_)) => "protocol error".into(),
        Some(ExecError::NotConnected) => "not connected".into(),
        Some(ExecError::Cancelled) => "cancelled".into(),
        Some(ExecError::Other(_)) | None => "other".into(),
    }
}

async fn run(
    ex: &Executor,
    command: &[&str],
    timeout: Duration,
    idle: Option<Duration>,
) -> anyhow::Result<CommandResult> {
    ex.run(command, timeout, idle).await
}

fn exit(r: &CommandResult) -> String {
    r.exit_code.map_or("null".into(), |c| c.to_string())
}

/// T21: stream boundaries, the shared idle clock and backpressure
pub async fn t21(t: &Target, ex: &Executor) {
    // Fixed read boundaries distinguish decoder semantics from transport packetisation.
    for (label, chunks) in [
        ("utf8_split", vec![vec![0xe2], vec![0x82, 0xac, b'\n']]),
        ("crlf_split", vec![b"left\r".to_vec(), b"\nright\n".to_vec()]),
    ] {
        let mut buffer = crate::lines::LineBuffer::default();
        let mut lines = Vec::new();
        for chunk in chunks {
            buffer.append(&chunk);
            lines.extend(buffer.flush());
        }
        lines.extend(buffer.drain());
        out(t, &format!("T21.{label}"), crate::exec::kotlin_list(&lines));
    }

    match run(ex, &["sh", "-c", "yes ä漢字€ | head -n 100000"], S(60), None).await {
        Ok(r) => {
            out(t, "T21.utf8", format!("exit={} lines={}", exit(&r), r.stdout.len()));
            let corrupted = r.stdout.iter().filter(|l| l.as_str() != "ä漢字€").count();
            out(t, "T21.info.utf8_corrupted_lines", corrupted);
        }
        Err(e) => out(t, "T21.utf8", format!("failed: {}", kind(&e))),
    }
    let crlf = ["awk", "BEGIN{for(i=0;i<100000;i++) printf \"l%d\\r\\n\", i}"];
    match run(ex, &crlf, S(60), None).await {
        Ok(r) => {
            let empty = r.stdout.iter().filter(|l| l.is_empty()).count();
            let ordered = r.stdout.iter().enumerate().all(|(i, l)| *l == format!("l{i}"));
            out(
                t,
                "T21.crlf",
                format!(
                    "exit={} lines={} empty={empty} ordered={ordered}",
                    exit(&r),
                    r.stdout.len()
                ),
            );
        }
        Err(e) => out(t, "T21.crlf", format!("failed: {}", kind(&e))),
    }
    let asymmetric = [
        "sh",
        "-c",
        "for i in 1 2 3 4 5 6; do echo e$i 1>&2; sleep 0.5; done; echo out",
    ];
    match run(ex, &asymmetric, S(30), Some(S(1))).await {
        Ok(r) => out(t, "T21.asymmetric_idle", r.show()),
        Err(e) => out(t, "T21.asymmetric_idle", format!("failed: {}", kind(&e))),
    }
    // Nobody reads for 5 s: the producer blocks once the buffers and the SSH window are full
    let command = strings(&[
        "sh",
        "-c",
        "s=$(date +%s); seq 1 1000000; echo blocked_s=$(( $(date +%s) - s ))",
    ]);
    match ex.execute(&command, S(60), None).await {
        Ok(mut session) => {
            tokio::time::sleep(S(5)).await;
            match session.await_result().await {
                Ok(r) => {
                    let last = r.stdout.last().cloned().unwrap_or_default();
                    let blocked: u64 = last.trim_start_matches("blocked_s=").parse().unwrap_or(0);
                    out(
                        t,
                        "T21.backpressure",
                        format!(
                            "exit={} lines={} producer_blocked={}",
                            exit(&r),
                            r.stdout.len(),
                            blocked >= 4
                        ),
                    );
                    out(t, "T21.info.backpressure_blocked_s", blocked);
                }
                Err(e) => out(t, "T21.backpressure", format!("failed: {e}")),
            }
        }
        Err(e) => out(t, "T21.backpressure", format!("failed: {}", kind(&e))),
    }
}

/// T22: cancelling the exit wait, and the isAlive snapshot
pub async fn t22(t: &Target, ex: &Executor) {
    let command = strings(&["sh", "-c", "sleep 3.3; echo late"]);
    let mut session = match ex.execute(&command, S(30), None).await {
        Ok(session) => session,
        Err(e) => return out(t, "T22.cancelled_await", format!("failed: {}", kind(&e))),
    };
    let cancelled = tokio::time::timeout(S(1), session.await_result()).await.is_err();
    out(t, "T22.await_cancelled", cancelled);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let running = run(ex, &["pgrep", "-f", "[s]leep 3.3"], S(10), None)
        .await
        .map(|r| r.exit_code == Some(0))
        .unwrap_or(false);
    out(t, "T22.remote_still_running_after_cancel", running);
    tokio::time::sleep(S(4)).await;
    let start = Instant::now();
    let outcome = match tokio::time::timeout(S(45), session.close()).await {
        Ok(Ok(())) => "returned".to_string(),
        Ok(Err(ExecError::CloseTimeout(_))) => "failed: close timeout".to_string(),
        Ok(Err(e)) => format!("failed: {e}"),
        Err(_) => "still waiting after 45 s".to_string(),
    };
    out(t, "T22.close_after_cancel", outcome);
    out(t, "T22.info.close_after_cancel_ms", start.elapsed().as_millis());
    out(
        t,
        "T22.after",
        run(ex, &["echo", "hello"], S(10), None)
            .await
            .map(|r| r.show())
            .unwrap_or_default(),
    );

    match ex.execute(&strings(&["echo", "done"]), S(10), None).await {
        Ok(mut session) => {
            let result = session.await_result().await.map(|r| r.show()).unwrap_or_default();
            out(t, "T22.finished", result);
            out(t, "T22.is_alive_after_exit", session.is_alive());
        }
        Err(e) => out(t, "T22.finished", format!("failed: {}", kind(&e))),
    }
}

/// T23 (fake `--reject-exec`): the server answers the exec request with failure
pub async fn t23(t: &Target, ex: &Executor) {
    match ex.execute(&strings(&["echo", "x"]), S(10), None).await {
        Ok(mut session) => out(
            t,
            "T23.execute",
            format!("started; await: {:?}", session.await_result().await.map(|r| r.show())),
        ),
        Err(e) => {
            out(t, "T23.execute", format!("failed: {}", kind(&e)));
            out(t, "T23.info.error", format!("{e:#}"));
        }
    }
    out(
        t,
        "T23.safe_execute",
        ex.safe_execute(&strings(&["echo", "x"]), S(10))
            .await
            .map_or("null".into(), |r| r.show()),
    );
    out(t, "T23.connected", ex.connected());
}

/// T24 (fake `--disconnect-on-exec=<reason>`): the server disconnects instead of running the command
pub async fn t24(t: &Target, ex: &Executor) {
    let start = Instant::now();
    let result = match ex.execute(&strings(&["sleep", "5"]), S(30), None).await {
        Ok(mut session) => session.await_result().await.map_err(anyhow::Error::from),
        Err(e) => Err(e),
    };
    match result {
        Ok(r) => out(t, "T24.result", r.show()),
        Err(e) => {
            out(t, "T24.result", format!("failed: {}", kind(&e)));
            out(t, "T24.info.error", format!("{e:#}"));
        }
    }
    out(t, "T24.info.elapsed_ms", start.elapsed().as_millis());
    tokio::time::sleep(Duration::from_millis(200)).await;
    out(t, "T24.connected", ex.connected());
}

/// T25 (fake built with the keep-alive no-reply patch): output keeps flowing while keep-alive requests go unanswered
pub async fn t25(t: &Target, ex: &Executor) {
    let start = Instant::now();
    let command = strings(&["sh", "-c", "while true; do echo tick; sleep 0.2; done"]);
    let result = match ex.execute(&command, S(12), None).await {
        Ok(mut session) => session.await_result().await.map_err(anyhow::Error::from),
        Err(e) => Err(e),
    };
    let elapsed = start.elapsed();
    // The command timeout is 12 s; a loss reported before it happened while output was still flowing
    out(
        t,
        "T25.lost_while_output_flowed",
        result.is_err() && elapsed < Duration::from_secs(11),
    );
    match result {
        Ok(r) => {
            out(t, "T25.result", format!("exit={}", exit(&r)));
            out(t, "T25.info.lines", r.stdout.len());
        }
        Err(e) => {
            out(t, "T25.result", format!("failed: {}", kind(&e)));
            out(t, "T25.info.error", format!("{e:#}"));
        }
    }
    out(t, "T25.info.elapsed_ms", elapsed.as_millis());
    out(t, "T25.connected", ex.connected());
}
