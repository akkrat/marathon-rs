//! Port of vendor/vendor-apple/ios/src/test/kotlin/com/malinskiy/marathon/apple/ios/listener/video/AwaitRemovalTest.kt
mod support;
use marathon_apple::cmd::SessionScope;
use marathon_ios::listener::video::await_removal;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn check(gone_after: usize, expected_reads: usize, elapsed: i64, result: bool) {
    let clock = Clock::default();
    let reads = Arc::new(AtomicUsize::new(0));
    let waiting = AtomicUsize::new(0);
    let r = reads.clone();
    let actual = await_removal(
        &clock,
        Arc::new(SessionScope::default()),
        10_000,
        100,
        || {
            waiting.fetch_add(1, Ordering::SeqCst);
        },
        move |_| {
            let n = r.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok((n < gone_after).then(|| "123".into())) })
        },
    )
    .await
    .unwrap();
    assert_eq!(actual, result);
    assert_eq!(reads.load(Ordering::SeqCst), expected_reads);
    assert_eq!(clock.now(), elapsed);
    assert_eq!(waiting.load(Ordering::SeqCst), usize::from(gone_after > 0));
}
#[tokio::test(start_paused = true)]
async fn returns_right_away_when_the_file_is_already_gone() {
    check(0, 1, 0, true).await
}
#[tokio::test(start_paused = true)]
async fn polls_once_per_interval_until_the_file_is_removed() {
    check(4, 5, 400, true).await
}
#[tokio::test(start_paused = true)]
async fn gives_up_after_the_timeout() {
    check(usize::MAX, 100, 10_000, false).await
}
#[tokio::test(start_paused = true)]
async fn approved_blank_pidfile_is_already_gone() {
    let clock = Clock::default();
    assert!(
        await_removal(
            &clock,
            Arc::new(SessionScope::default()),
            10_000,
            100,
            || panic!("must not wait"),
            |_| Box::pin(async { Ok(Some("\u{a0}\t".into())) })
        )
        .await
        .unwrap()
    );
    assert_eq!(clock.now(), 0);
}

// Timeout is registered before delay in Kotlin's scheduler. Prioritize expiry at
// the shared 10-second deadline, before the next read is allowed to run.
#[derive(Default)]
struct Clock(support::Clock);
impl Clock {
    fn now(&self) -> i64 {
        self.0.now()
    }
}
impl marathon_core::runtime::VendorClock for Clock {
    fn delay(
        &self,
        millis: i64,
        signal: Arc<dyn marathon_core::runtime::CancellationSignal>,
    ) -> marathon_core::runtime::VendorFuture<'_, ()> {
        self.0.delay(millis, signal)
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, ()> {
        self.0.with_timeout(parent, millis, body)
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn marathon_core::runtime::CancellationSignal>,
        millis: i64,
        body: marathon_core::runtime::VendorTimeoutBody<'a>,
    ) -> marathon_core::runtime::VendorFuture<'a, Option<()>> {
        Box::pin(async move {
            let child = Arc::new(SessionScope::default());
            let mut future = body(child.clone());
            tokio::select! { biased;
                reason = parent.cancelled() => { child.cancel(reason.clone()); let _ = future.await; Err(marathon_core::error::VendorError::Cancelled(reason)) },
                _ = tokio::time::sleep(std::time::Duration::from_millis(millis as u64)) => { child.cancel(marathon_apple::cmd::cancelled("timeout")); let _ = future.await; Ok(None) },
                result = &mut future => result.map(|()| Some(())),
            }
        })
    }
}
