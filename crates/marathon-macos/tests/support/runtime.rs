//! Test runtime and virtual-time clock reused from the Apple session parity harness.
#![allow(dead_code)]
use jcompat::jtime::{Duration, duration_of};
use marathon_apple::cmd::{SessionScope, failure};
use marathon_core::{
    error::{Cancellation, VendorError, VendorResult},
    runtime::{
        CancellationSignal, VendorClock, VendorFuture, VendorRuntime, VendorTask, VendorTaskBody, VendorTimeoutBody,
    },
};
use marathon_error::{FailureKind, InjectedKind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;

pub fn duration(millis: i64) -> Duration {
    duration_of(millis.div_euclid(1000), millis.rem_euclid(1000) * 1_000_000).unwrap()
}
#[derive(Default)]
pub struct Runtime;
struct Task(watch::Sender<Option<VendorResult<()>>>);
impl VendorTask for Task {
    fn cancel(&self, _: Cancellation) { /* test tasks use their injected session signal */
    }
    fn join(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let mut rx = self.0.subscribe();
            loop {
                if let Some(result) = rx.borrow().clone() {
                    return result;
                }
                rx.changed().await.map_err(|e| {
                    VendorError::Other(failure(FailureKind::Injected(InjectedKind::TestTask), e.to_string()))
                })?;
            }
        })
    }
}
impl VendorRuntime for Runtime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        // Test tasks ignore cancel(), as in spawn; the body still gets its own signal.
        self.spawn(body(Arc::new(SessionScope::default())))
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        let task = Arc::new(Task(watch::channel(None).0));
        let complete = task.clone();
        tokio::spawn(async move {
            complete.0.send_replace(Some(body.await));
        });
        task
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        let task = Arc::new(Task(watch::channel(None).0));
        let complete = task.clone();
        tokio::task::spawn_blocking(move || {
            complete.0.send_replace(Some(body()));
        });
        task
    }
}
pub struct Clock {
    start: tokio::time::Instant,
}
impl Default for Clock {
    fn default() -> Self {
        Self {
            start: tokio::time::Instant::now(),
        }
    }
}
impl Clock {
    pub fn now(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }
}
static SCOPE: AtomicU64 = AtomicU64::new(1);
impl VendorClock for Clock {
    fn delay(&self, millis: i64, cancellation: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            if let Some(reason) = cancellation.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            if millis <= 0 {
                return Ok(());
            }
            tokio::select! { biased;
                reason = cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
                _ = tokio::time::sleep(std::time::Duration::from_millis(millis as u64)) => Ok(()),
            }
        })
    }
    fn with_timeout<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let scope = SCOPE.fetch_add(1, Ordering::SeqCst);
            let child = Arc::new(SessionScope::default());
            let expired = Cancellation::Timeout {
                scope,
                detail: failure(FailureKind::Injected(InjectedKind::TimeoutCancellation), "timeout"),
            };
            if millis <= 0 {
                return Err(VendorError::Cancelled(expired));
            }
            let mut future = body(child.clone());
            tokio::select! { biased;
                reason = parent.cancelled() => { child.cancel(reason.clone()); let _ = future.await; Err(VendorError::Cancelled(reason)) },
                result = &mut future => result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(millis as u64)) => {
                    child.cancel(expired.clone()); let _ = future.await; Err(VendorError::Cancelled(expired))
                },
            }
        })
    }
    fn with_timeout_or_null<'a>(
        &'a self,
        parent: Arc<dyn CancellationSignal>,
        millis: i64,
        body: VendorTimeoutBody<'a>,
    ) -> VendorFuture<'a, Option<()>> {
        Box::pin(async move {
            // Remember this body's signal, so only this invocation's expiry is caught.
            let own = Arc::new(Mutex::new(None));
            let observed = own.clone();
            let result = self
                .with_timeout(
                    parent,
                    millis,
                    Box::new(move |child| {
                        *observed.lock().unwrap() = Some(child.clone());
                        body(child)
                    }),
                )
                .await;
            match result {
                Ok(()) => Ok(Some(())),
                Err(VendorError::Cancelled(reason @ Cancellation::Timeout { .. }))
                    if millis <= 0
                        || own
                            .lock()
                            .unwrap()
                            .as_ref()
                            .and_then(|s: &Arc<dyn CancellationSignal>| s.reason())
                            .as_ref()
                            == Some(&reason) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
    }
}
