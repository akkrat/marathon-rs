//! Vendor tasks of an SSH connection end with it (review round 1, R1-sol-3): a cancelled connect, a close and a
//! transport death leave no keep-alive, reaper or ping task running on the vendor runtime.
mod support;
use fake_worker::script::{Disconnect, Faults, Script};
use marathon_apple::cmd::{SessionScope, remote::ssh::sshj::SshjCommandExecutorFactory};
use marathon_core::{
    error::VendorResult,
    runtime::{VendorFuture, VendorRuntime, VendorTask, VendorTaskBody},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
use support::ssh;

/// The test runtime, counting the vendor tasks that have not finished.
#[derive(Clone, Default)]
struct Counting(Arc<AtomicUsize>);
struct Running(Arc<AtomicUsize>);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl VendorRuntime for Counting {
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let running = Running(self.0.clone());
        support::Runtime.spawn(Box::pin(async move {
            let _running = running;
            body.await
        }))
    }
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        self.spawn(body(Arc::new(SessionScope::default())))
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        support::Runtime.spawn_blocking(body)
    }
}
impl Counting {
    fn factory(&self) -> SshjCommandExecutorFactory {
        let (logging, _) = ssh::logging(false);
        SshjCommandExecutorFactory::new(logging, Arc::new(self.clone()), Arc::new(support::Clock::default()))
    }
    /// The running task count once it reaches zero, or after 60 s.
    async fn settled(&self) -> usize {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.0.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_connect_leaves_no_vendor_task() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let runtime = Counting::default();
    let factory = runtime.factory();
    let mut connecting = Box::pin(factory.connect("127.0.0.1", fake.port(), ssh::password(), None, false));
    std::future::poll_fn(|cx| {
        assert!(connecting.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(connecting);
    assert_eq!(runtime.settled().await, 0, "the undelivered connection was not closed");
    assert_eq!(fake.events_at_least("connection_end", 1).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn close_leaves_no_vendor_task() {
    let fake = ssh::fake(ssh::suite_script(), 4).await;
    let runtime = Counting::default();
    let executor = ssh::connect_with(&fake, &runtime.factory(), false).await.unwrap();
    executor.connection().close().await.unwrap();
    assert_eq!(runtime.settled().await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn transport_death_ends_the_keep_alive() {
    let fake = ssh::fake(
        Script {
            faults: Faults {
                after_auth: Some(Disconnect {
                    reason: 11,
                    message: "fixture disconnect 11".into(),
                }),
                ..Default::default()
            },
            ..ssh::suite_script()
        },
        4,
    )
    .await;
    let runtime = Counting::default();
    // Still owned and never closed: only the transport's death can end its tasks.
    let executor = ssh::connect_with(&fake, &runtime.factory(), false).await.unwrap();
    assert_eq!(runtime.settled().await, 0, "a task outlived the transport");
    assert!(!executor.connection().is_connected());
}
