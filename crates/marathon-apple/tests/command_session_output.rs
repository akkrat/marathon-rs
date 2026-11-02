//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/CommandSessionOutputTest.kt
mod support;
use marathon_apple::cmd::{SessionScope, cancelled, consume_output, failure};
use marathon_core::error::VendorError;
use std::sync::Arc;
use support::*;
#[tokio::test]
async fn drains_stderr_beyond_channel_capacity_while_stdout_is_open() {
    watchdog(async {
        let session = Arc::new(fake());
        let producer = session.clone();
        let task = tokio::spawn(async move {
            producer.stdout.send("start".into()).await.unwrap();
            for i in 0..10 {
                producer.stderr.send(format!("diagnostic {i}")).await.unwrap();
            }
            producer.stdout.send("end".into()).await.unwrap();
            producer.stdout.close(None);
            producer.stderr.close(None);
            producer.exit_code.complete(Ok(Some(0)));
        });
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = bounded(
            5,
            consume_output(
                &session,
                &context(),
                |line| {
                    out.push(line);
                    std::future::ready(Ok(()))
                },
                |line| {
                    err.push(line);
                    std::future::ready(Ok(()))
                },
            ),
        )
        .await
        .unwrap();
        task.await.unwrap();
        assert_eq!(code, Some(0));
        assert_eq!(out, ["start", "end"]);
        assert_eq!(err, (0..10).map(|i| format!("diagnostic {i}")).collect::<Vec<_>>());
    })
    .await;
}
#[tokio::test]
async fn waits_for_exit_code_after_streams_are_closed() {
    watchdog(async {
        let session = Arc::new(fake());
        session.stdout.close(None);
        session.stderr.close(None);
        let consumer = session.clone();
        let task = tokio::spawn(async move {
            bounded(
                5,
                consume_output(&consumer, &context(), |_| async { Ok(()) }, |_| async { Ok(()) }),
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        session.exit_code.complete(Ok(Some(65)));
        assert_eq!(task.await.unwrap().unwrap(), Some(65));
    })
    .await;
}
#[tokio::test]
async fn stream_failure_cancels_other_consumer_and_is_rethrown() {
    watchdog(async {
        let session = fake();
        let cause = failure("java.util.concurrent.TimeoutException", "idle timeout");
        session.stdout.close(Some(cause.clone()));
        assert_eq!(
            bounded(
                5,
                consume_output(&session, &context(), |_| async { Ok(()) }, |_| async { Ok(()) })
            )
            .await,
            Err(VendorError::Other(cause))
        );
        assert!(session.stderr.send("unused".into()).await.is_err());
    })
    .await;
}
#[tokio::test]
async fn callback_failure_cancels_other_consumer_and_is_rethrown() {
    watchdog(async {
        let session = fake();
        session.stderr.send("boom".into()).await.unwrap();
        let error = VendorError::Other(failure("java.lang.IllegalStateException", "boom"));
        assert_eq!(
            bounded(
                5,
                consume_output(
                    &session,
                    &context(),
                    |_| async { Ok(()) },
                    |line| async move { Err(VendorError::Other(failure("java.lang.IllegalStateException", line))) }
                )
            )
            .await,
            Err(error)
        );
        assert!(session.stdout.send("unused".into()).await.is_err());
        assert!(session.stderr.send("unused".into()).await.is_err());
    })
    .await;
}
#[tokio::test]
async fn cancelled_stream_cancels_other_consumer_and_is_rethrown() {
    watchdog(async {
        let session = fake();
        let reason = cancelled("stream cancelled");
        session.stdout.cancel(reason.clone());
        assert_eq!(
            bounded(
                5,
                consume_output(&session, &context(), |_| async { Ok(()) }, |_| async { Ok(()) })
            )
            .await,
            Err(VendorError::Cancelled(reason))
        );
        assert!(session.stderr.is_closed());
    })
    .await;
}
#[tokio::test]
async fn callback_cancellation_cancels_other_consumer_and_is_rethrown() {
    watchdog(async {
        let session = fake();
        session.stderr.send("line".into()).await.unwrap();
        assert_eq!(
            bounded(
                5,
                consume_output(
                    &session,
                    &context(),
                    |_| async { Ok(()) },
                    |_| async { Err(VendorError::Cancelled(cancelled("callback cancelled"))) }
                )
            )
            .await,
            Err(VendorError::Cancelled(cancelled("callback cancelled")))
        );
        assert!(session.stdout.is_closed());
        assert!(session.stderr.is_closed());
    })
    .await;
}
#[tokio::test]
async fn cancellation_stops_both_consumers() {
    watchdog(async {
        let session = Arc::new(fake());
        session.stdout.send("line".into()).await.unwrap();
        let consuming = session.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut started = Some(started);
        let task = tokio::spawn(async move {
            bounded(
                5,
                consume_output(
                    &consuming,
                    &context(),
                    |_| {
                        if let Some(started) = started.take() {
                            let _ = started.send(());
                        }
                        std::future::pending()
                    },
                    |_| async { Ok(()) },
                ),
            )
            .await
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(session.stdout.is_closed());
        assert!(session.stderr.is_closed());
    })
    .await;
}
#[tokio::test]
async fn already_cancelled_caller_cancels_channels_before_consumers_start() {
    watchdog(async {
        let session = fake();
        let signal = Arc::new(SessionScope::default());
        signal.cancel(cancelled("already cancelled"));
        let mut ctx = context();
        ctx.cancellation = signal;
        assert!(
            bounded(
                5,
                consume_output(
                    &session,
                    &ctx,
                    |_| async { panic!("must not start") },
                    |_| async { panic!("must not start") }
                )
            )
            .await
            .is_err()
        );
        assert!(session.stdout.is_closed());
        assert!(session.stderr.is_closed());
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn timeout_cancels_both_channels() {
    watchdog(async {
        let session = fake();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                consume_output(&session, &context(), |_| async { Ok(()) }, |_| async { Ok(()) })
            )
            .await
            .is_err()
        );
        assert!(session.stdout.is_closed());
        assert!(session.stderr.is_closed());
    })
    .await;
}
#[tokio::test]
async fn cancelling_a_full_receiver_releases_its_blocked_producer() {
    watchdog(async {
        let session = fake();
        session.stdout.send("first".into()).await.unwrap();
        let output = session.stdout.clone();
        let task = tokio::spawn(async move { output.send("blocked".into()).await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        session.stdout.cancel(cancelled("no consumer"));
        assert_eq!(
            task.await.unwrap(),
            Err(VendorError::Cancelled(cancelled("no consumer")))
        );
    })
    .await;
}

#[tokio::test]
async fn drain_collectors_discard_output_after_drain_has_returned() {
    watchdog(async {
        let session = fake();
        let ctx = context();
        let mut draining = Box::pin(session.drain(&ctx));
        std::future::poll_fn(|cx| {
            assert!(matches!(draining.as_mut().poll(cx), std::task::Poll::Ready(Ok(()))));
            std::task::Poll::Ready(())
        })
        .await;
        assert!(!session.stdout.is_closed());
        assert!(!session.stderr.is_closed());
        assert!(!session.exit_code.is_completed());
        for i in 0..100 {
            session.stdout.send(format!("stdout {i}")).await.unwrap();
            session.stderr.send(format!("stderr {i}")).await.unwrap();
        }
        session.stdout.close(Some(failure(
            "java.util.concurrent.TimeoutException",
            "later idle timeout",
        )));
        session.stderr.close(None);
        session.close().await.unwrap();
    })
    .await;
}
