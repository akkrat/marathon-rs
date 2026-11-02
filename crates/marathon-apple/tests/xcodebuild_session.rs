//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/XcodebuildSession.kt (state-machine probe replay).
mod support;
use jcompat::logging::MarathonLogging;
use marathon_apple::{
    cmd::{BaseCommand, CommandSession, SessionControl, remote::ssh::sshj::config::SshLogging},
    xcodebuild_session::XcodebuildSession,
};
use std::sync::{Arc, Mutex};
fn logging() -> SshLogging {
    SshLogging::new(
        Arc::new(Mutex::new(MarathonLogging::default())),
        Arc::new(|_, _, _, _| {}),
    )
}
#[tokio::test]
async fn stop_before_produce_closes_but_await_never_stops() {
    let s = XcodebuildSession::new(Arc::new(support::Runtime), logging());
    let ctx = support::context();
    assert!(!s.closed());
    assert!(!s.await_closed(support::duration(1), &ctx).await.unwrap());
    assert!(s.stop(support::duration(10), &ctx).await.is_none());
    assert!(s.closed());
    assert!(!s.abandoned());
}
#[tokio::test]
async fn abandoned_late_close_becomes_closed_and_remains_joinable() {
    let s = XcodebuildSession::new(Arc::new(support::Runtime), logging());
    let ctx = support::context();
    let release = Arc::new(tokio::sync::Notify::new());
    let closing = Arc::new(tokio::sync::Notify::new());
    let done = closing.clone();
    let go = release.clone();
    let command = CommandSession::new(
        BaseCommand::new(1, Arc::new(support::Runtime)),
        SessionControl {
            is_alive: Box::new(|| false),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(move || {
                let done = done.clone();
                let go = go.clone();
                Box::pin(async move {
                    done.notify_one();
                    go.notified().await;
                    Ok(())
                })
            }),
        },
    );
    let channel = s.produce(
        ctx.clone(),
        move |_| async move { Ok(command) },
        |_, _, _| async { Ok(()) },
    );
    assert!(channel.receive(&ctx).await.unwrap().is_none());
    closing.notified().await;
    assert!(!s.closed());
    assert!(s.stop(support::duration(1), &ctx).await.is_some());
    assert!(s.abandoned());
    assert!(s.check_closed("serial").is_err());
    release.notify_one();
    s.join_producer().await.unwrap();
    assert!(s.closed());
    assert!(s.abandoned());
    assert!(s.check_closed("serial").is_ok());
}

#[tokio::test]
async fn receiver_cancellation_retains_the_first_terminal_result() -> Result<(), String> {
    use marathon_apple::cmd::{CommandResult, failure};
    use marathon_core::error::VendorError;
    let mut results = vec![];
    for failed in [false, true] {
        let runtime = Arc::new(support::Runtime);
        let session = XcodebuildSession::new(runtime.clone(), logging());
        let context = support::context();
        let command = CommandSession::finished(
            runtime,
            CommandResult {
                stdout: vec![],
                stderr: vec![],
                exit_code: Some(0),
            },
        );
        let expected = VendorError::Other(failure("java.io.IOException", "output failed"));
        let error = expected.clone();
        let channel = session.produce(
            context.clone(),
            move |_| async move { Ok(command) },
            move |_, _, _| async move { if failed { Err(error) } else { Ok(()) } },
        );
        let before = channel.receive(&context).await;
        channel.cancel();
        let after = channel.receive(&context).await;
        if let Some(error) = session.stop(support::duration(1000), &context).await {
            return Err(error.to_java_string().to_string_lossy());
        }
        session
            .join_producer()
            .await
            .map_err(|e| e.failure().to_java_string().to_string_lossy())?;
        results.push(serde_json::json!({"failed":failed,"before":before.as_ref().err().map(|e|e.failure().to_java_string().to_string_lossy()).unwrap_or_else(||"closed".into()),"after":after.as_ref().err().map(|e|e.failure().to_java_string().to_string_lossy()).unwrap_or_else(||"closed".into()),"closed":session.closed()}));
        if failed {
            assert_eq!(before, Err(expected.clone()));
            assert_eq!(after, Err(expected));
        } else {
            assert_eq!(before, Ok(None));
            assert_eq!(after, Ok(None));
        }
    }
    let row = include_str!("../../../fixtures/oracle/test-runner/probes.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["case"] == "session-channel-cancel")
        .unwrap();
    assert_eq!(serde_json::json!(results), row["results"]);
    Ok(())
}

#[tokio::test]
async fn reader_cancellation_preserves_a_published_terminal_failure() -> Result<(), String> {
    use marathon_apple::cmd::{CommandResult, SessionScope, cancelled, failure};
    use marathon_core::error::VendorError;
    let runtime = Arc::new(support::Runtime);
    let session = XcodebuildSession::new(runtime.clone(), logging());
    let command = CommandSession::finished(
        runtime,
        CommandResult {
            stdout: vec![],
            stderr: vec![],
            exit_code: Some(0),
        },
    );
    let mut context = support::context();
    let expected = VendorError::Other(failure("java.io.IOException", "boom"));
    let error = expected.clone();
    let channel = session.produce(
        context.clone(),
        move |_| async move { Ok(command) },
        move |_, _, _| async move { Err(error) },
    );
    session.join_producer().await.map_err(|e| format!("{e:?}"))?;
    let signal = Arc::new(SessionScope::default());
    signal.cancel(cancelled("reader cancelled"));
    context.cancellation = signal;
    assert_eq!(channel.receive(&context).await, Err(expected));
    channel.cancel();
    assert!(session.stop(support::duration(1000), &context).await.is_none());
    Ok(())
}

#[tokio::test]
async fn reader_cancellation_preserves_buffered_events_but_cancels_a_suspending_receive() -> Result<(), String> {
    use marathon_apple::{
        cmd::{CommandResult, SessionScope, cancelled},
        test::TestEvent,
    };
    use marathon_core::error::VendorError;
    let runtime = Arc::new(support::Runtime);
    let session = XcodebuildSession::new(runtime.clone(), logging());
    let mut context = support::context();
    let sent = Arc::new(tokio::sync::Notify::new());
    let ready = sent.clone();
    let command = CommandSession::finished(
        runtime,
        CommandResult {
            stdout: vec![],
            stderr: vec![],
            exit_code: Some(0),
        },
    );
    let events = vec![TestEvent::TestRunFailed {
        message: "queued failure".into(),
        reason: marathon_apple::logparser::parser::DeviceFailureReason::Unknown,
    }];
    let buffered = events.clone();
    let channel = session.produce(
        context.clone(),
        move |_| async move { Ok(command) },
        move |_, sender, ctx| async move {
            sender.send(buffered).await?;
            ready.notify_one();
            Err(VendorError::Cancelled(ctx.cancellation.cancelled().await))
        },
    );
    sent.notified().await;
    let signal = Arc::new(SessionScope::default());
    let reason = cancelled("reader cancelled");
    signal.cancel(reason.clone());
    context.cancellation = signal;
    assert_eq!(channel.receive(&context).await, Ok(Some(events)));
    assert_eq!(channel.receive(&context).await, Err(VendorError::Cancelled(reason)));
    channel.cancel();
    assert!(session.stop(support::duration(1000), &context).await.is_none());
    session.join_producer().await.map_err(|e| format!("{e:?}"))?;
    Ok(())
}

#[tokio::test]
async fn dropping_a_receiver_does_not_close_a_blocked_producer_but_stop_does() -> Result<(), String> {
    use marathon_apple::test::TestEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runtime = Arc::new(support::Runtime);
    let session = XcodebuildSession::new(runtime.clone(), logging());
    let context = support::context();
    let closes = Arc::new(AtomicUsize::new(0));
    let closed = closes.clone();
    let command = CommandSession::new(
        BaseCommand::new(1, runtime),
        SessionControl {
            is_alive: Box::new(|| true),
            terminate: Box::new(|| Ok(())),
            kill: Box::new(|| Ok(())),
            close: Box::new(move || {
                let closed = closed.clone();
                Box::pin(async move {
                    closed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        },
    );
    let blocked = Arc::new(tokio::sync::Notify::new());
    let ready = blocked.clone();
    let channel = session.produce(
        context.clone(),
        move |_| async move { Ok(command) },
        move |_, sender, _| async move {
            sender.send(vec![TestEvent::TestRunStartedEvent]).await?;
            ready.notify_one();
            sender.send(vec![TestEvent::TestRunEnded]).await
        },
    );
    blocked.notified().await;
    drop(channel);
    tokio::task::yield_now().await;
    assert_eq!(closes.load(Ordering::SeqCst), 0);
    assert!(session.stop(support::duration(1000), &context).await.is_none());
    session.join_producer().await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert!(session.closed());
    Ok(())
}
