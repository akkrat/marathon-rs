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
