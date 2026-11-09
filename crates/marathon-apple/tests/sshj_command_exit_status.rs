//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/remote/ssh/sshj/SshjCommandExitStatusTest.kt
//! runTest's virtual time is Tokio's paused clock behind the test VendorClock.
mod support;
use marathon_apple::cmd::{SessionScope, remote::ssh::sshj::executor::suspend_for};
use marathon_core::runtime::VendorClock;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use support::{Clock, exit_state::Command};

#[tokio::test(start_paused = true)]
async fn waiting_for_the_exit_status_joins_the_channel_only_once_it_is_no_longer_open() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let command = Command::new(move || counted.fetch_add(1, Ordering::SeqCst) < 50, Some(3));
    let clock = Clock::default();
    assert_eq!(
        suspend_for(&command, &clock, Arc::new(SessionScope::default())).await,
        Ok(Some(3))
    );
    assert_eq!(polls.load(Ordering::SeqCst), 51);
    assert_eq!(command.joins.load(Ordering::SeqCst), 1);
    assert_eq!(command.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_the_exit_status_backs_off_while_the_command_runs() {
    let polls = Arc::new(AtomicUsize::new(0));
    let counted = polls.clone();
    let start = tokio::time::Instant::now();
    let command = Command::new(
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
            start.elapsed() < std::time::Duration::from_millis(10_000)
        },
        Some(0),
    );
    let clock = Clock::default();
    assert_eq!(
        suspend_for(&command, &clock, Arc::new(SessionScope::default())).await,
        Ok(Some(0))
    );
    // polls at 0, 1, 3, 7, 15 ms, then every 20 ms instead of every millisecond
    assert!(
        (500..=510).contains(&polls.load(Ordering::SeqCst)),
        "{}",
        polls.load(Ordering::SeqCst)
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_wait_leaves_the_command_alone() {
    let command = Arc::new(Command::new(|| true, None));
    let clock = Arc::new(Clock::default());
    let waiting = command.clone();
    let inner = clock.clone();
    let result = Arc::new(Mutex::new(None));
    let seen = result.clone();
    let outcome = clock
        .with_timeout_or_null(
            Arc::new(SessionScope::default()),
            1_000,
            Box::new(move |child| {
                Box::pin(async move {
                    *seen.lock().unwrap() = Some(suspend_for(waiting.as_ref(), inner.as_ref(), child).await);
                    Ok(())
                })
            }),
        )
        .await;
    assert_eq!(outcome, Ok(None));
    assert_eq!(command.joins.load(Ordering::SeqCst), 0);
    assert_eq!(command.closes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_clock_failure_message_uses_display_and_keeps_external_message_text() {
    use marathon_core::{
        error::{Failure, FailureKind, VendorError},
        runtime::{CancellationSignal, VendorFuture, VendorTimeoutBody},
    };
    struct FailingClock(Clock);
    impl VendorClock for FailingClock {
        fn delay(&self, _: i64, _: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
            Box::pin(async {
                Err(VendorError::Other(Failure::from_kind(
                    FailureKind::IllegalState,
                    Some("external java.io.IOException: remote".into()),
                )))
            })
        }
        fn with_timeout<'a>(
            &'a self,
            parent: Arc<dyn CancellationSignal>,
            millis: i64,
            body: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, ()> {
            self.0.with_timeout(parent, millis, body)
        }
        fn with_timeout_or_null<'a>(
            &'a self,
            parent: Arc<dyn CancellationSignal>,
            millis: i64,
            body: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, Option<()>> {
            self.0.with_timeout_or_null(parent, millis, body)
        }
    }
    let command = Command::new(|| true, None);
    let outcome = suspend_for(
        &command,
        &FailingClock(Clock::default()),
        Arc::new(SessionScope::default()),
    )
    .await;
    let Err(marathon_apple::cmd::remote::ssh::sshj::executor::ExitWaitError::Failed(failure)) = outcome else {
        panic!("clock failure did not propagate")
    };
    assert_eq!(failure.kind, FailureKind::Io);
    assert_eq!(
        failure.message().as_deref(),
        Some("invalid state: external java.io.IOException: remote")
    );
}
