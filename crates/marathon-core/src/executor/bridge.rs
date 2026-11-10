//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt
//! (suspending vendor calls from core coroutines, `:170`, `:191`).
use crate::{
    error::*,
    runtime::{CancellationSignal, VendorFuture, VendorRuntime},
};
use marathon_error::FailureKind;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::{Notify, oneshot};

/// Runs a vendor call off core as Kotlin's suspending call does on its dispatcher.
/// `cancellation` is the calling core task's signal, which the vendor observes
/// cooperatively. The result is the vendor's own outcome, awaited through its
/// cleanup even after cancellation: Kotlin's catch blocks classify what the
/// vendor threw, not why the caller was cancelled.
///
/// Kotlin calls the vendor inline in the calling coroutine, so the vendor's
/// first segment, up to its first suspension, runs before any coroutine
/// dispatched after the caller, such as the one that cancels it. The vendor
/// here starts later, on another runtime; it therefore gets a gated view of
/// `cancellation` that hides a cancellation requested after the call began
/// until the vendor's first poll has returned.
///
/// Await the returned future to completion: dropping it detaches the vendor
/// task, which runs on unobserved and loses its result.
pub async fn call_vendor<T: Send + 'static>(
    vendor: &dyn VendorRuntime,
    cancellation: Arc<dyn CancellationSignal>,
    call: impl FnOnce(Arc<dyn CancellationSignal>) -> VendorFuture<'static, T> + Send + 'static,
) -> VendorResult<T> {
    let (output, mut payload) = oneshot::channel();
    let gate = Arc::new(Gated {
        // A caller already cancelled at the call sees it at entry, as in Kotlin.
        armed: AtomicBool::new(cancellation.reason().is_some()),
        inner: cancellation,
        changed: Notify::new(),
    });
    let signal: Arc<dyn CancellationSignal> = gate.clone();
    let task = vendor.spawn(Box::pin(ArmAfterFirstPoll {
        body: Box::pin(async move {
            let value = call(signal).await?;
            let _ = output.send(value);
            Ok(())
        }),
        gate,
    }));
    // Protected join: the vendor task's own result, never the waiter's cancellation.
    task.join().await?;
    payload.try_recv().map_err(|_| {
        VendorError::Other(super::shared::failure(
            FailureKind::IllegalState,
            "vendor call finished without a result",
        ))
    })
}

struct Gated {
    inner: Arc<dyn CancellationSignal>,
    armed: AtomicBool,
    changed: Notify,
}
impl CancellationSignal for Gated {
    fn reason(&self) -> Option<Cancellation> {
        if self.armed.load(Ordering::Acquire) {
            self.inner.reason()
        } else {
            None
        }
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                let mut changed = std::pin::pin!(changed);
                changed.as_mut().enable();
                if self.armed.load(Ordering::Acquire) {
                    break;
                }
                changed.await;
            }
            self.inner.cancelled().await
        })
    }
}

/// Arms the gate once the vendor's first poll returns, the point where the
/// Kotlin vendor would first suspend; a vendor awaiting `cancelled()` then
/// observes a cancellation requested in between.
struct ArmAfterFirstPoll {
    body: VendorFuture<'static, ()>,
    gate: Arc<Gated>,
}
impl Future for ArmAfterFirstPoll {
    type Output = VendorResult<()>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Retained children must see cancellation even if the first poll unwinds.
        let _arm = ArmGate(&this.gate);
        this.body.as_mut().poll(cx)
    }
}
struct ArmGate<'a>(&'a Gated);
impl Drop for ArmGate<'_> {
    fn drop(&mut self) {
        if !self.0.armed.swap(true, Ordering::AcqRel) {
            // Unwinding assumes Tokio/replay wakers do not panic; a panicking waker here aborts.
            self.0.changed.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        executor::{SharedDeferred, shared::requested},
        runtime::{VendorTask, VendorTaskBody},
    };
    use futures_util::{FutureExt, task::noop_waker};
    use std::sync::Mutex;

    #[derive(Default)]
    struct ManualVendor {
        body: Mutex<Option<VendorFuture<'static, ()>>>,
        done: Arc<SharedDeferred<()>>,
    }
    struct ManualTask(Arc<SharedDeferred<()>>);
    impl VendorTask for ManualTask {
        fn cancel(&self, reason: Cancellation) {
            self.0.cancel(reason);
        }
        fn join(&self) -> VendorFuture<'_, ()> {
            Box::pin(async { self.0.wait().await.map(|_| ()) })
        }
    }
    impl VendorRuntime for ManualVendor {
        fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
            *self.body.lock().unwrap() = Some(body);
            Arc::new(ManualTask(self.done.clone()))
        }
        fn spawn_cancellable(&self, _: VendorTaskBody) -> Arc<dyn VendorTask> {
            unreachable!()
        }
        fn spawn_blocking(&self, _: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
            unreachable!()
        }
    }
    impl ManualVendor {
        fn poll_body(&self) -> Poll<VendorResult<()>> {
            let waker = noop_waker();
            self.body
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
        }
    }

    #[tokio::test]
    async fn first_poll_panic_releases_a_retained_childs_cancellation_gate() {
        let vendor = ManualVendor::default();
        let signal = super::super::Signal::new();
        let (child_out, child_in) = oneshot::channel();
        let mut call = Box::pin(call_vendor::<()>(&vendor, signal.clone(), move |signal| {
            Box::pin(async move {
                let child = tokio::spawn(async move { signal.cancelled().await });
                assert!(child_out.send(child).is_ok());
                panic!("first vendor poll");
            })
        }));
        assert!(call.as_mut().now_or_never().is_none());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vendor.poll_body()));
        assert!(panic.is_err());
        // A panicked poll poisoned only the test runtime's storage lock.
        vendor.body.lock().unwrap_or_else(|e| e.into_inner()).take();
        vendor
            .done
            .complete(Err(VendorError::Other(super::super::shared::failure(
                FailureKind::Runtime,
                "first vendor poll",
            ))));
        assert!(matches!(call.await, Err(VendorError::Other(_))));
        let mut child = child_in
            .await
            .unwrap_or_else(|_| panic!("child handle was not published"));
        let reason = requested("after panic");
        signal.cancel(reason.clone());
        // Bound the hang, then abort and join even on the defective path.
        let result = tokio::time::timeout(std::time::Duration::from_secs(60), &mut child).await;
        if result.is_err() {
            child.abort();
            let _ = child.await;
        }
        assert!(
            matches!(&result, Ok(Ok(observed)) if *observed == reason),
            "the retained child must observe cancellation after a first-poll panic: {result:?}"
        );
    }

    #[test]
    fn bridge_hides_new_cancellation_until_the_first_poll_returns() {
        let vendor = ManualVendor::default();
        let signal = super::super::Signal::new();
        let reason = requested("during dispatch");
        let expected = reason.clone();
        let mut call = Box::pin(call_vendor(&vendor, signal.clone(), move |signal| {
            Box::pin(async move {
                assert_eq!(signal.reason(), None);
                assert_eq!(signal.cancelled().await, expected);
                Ok(42)
            })
        }));
        assert!(call.as_mut().now_or_never().is_none());
        signal.cancel(reason);
        assert!(vendor.poll_body().is_pending());
        assert_eq!(vendor.poll_body(), Poll::Ready(Ok(())));
        // Sending the payload alone must not bypass the protected join.
        assert!(call.as_mut().now_or_never().is_none());
        vendor.done.complete(Ok(()));
        assert_eq!(call.as_mut().now_or_never(), Some(Ok(42)));
    }

    #[test]
    fn bridge_exposes_cancellation_already_present_at_entry() {
        let vendor = ManualVendor::default();
        let signal = super::super::Signal::new();
        let reason = requested("before dispatch");
        signal.cancel(reason.clone());
        let mut call = Box::pin(call_vendor(&vendor, signal, move |signal| {
            Box::pin(async move {
                assert_eq!(signal.reason(), Some(reason));
                Ok(7)
            })
        }));
        assert!(call.as_mut().now_or_never().is_none());
        assert_eq!(vendor.poll_body(), Poll::Ready(Ok(())));
        vendor.done.complete(Ok(()));
        assert_eq!(call.as_mut().now_or_never(), Some(Ok(7)));
    }

    #[test]
    fn bridge_rejects_successful_join_without_a_payload() {
        let vendor = ManualVendor::default();
        let mut call = Box::pin(call_vendor(&vendor, super::super::Signal::new(), |_| {
            Box::pin(async { Ok(7) })
        }));
        assert!(call.as_mut().now_or_never().is_none());
        vendor.body.lock().unwrap().take();
        vendor.done.complete(Ok(()));
        assert_eq!(
            call.as_mut().now_or_never(),
            Some(Err(VendorError::Other(super::super::shared::failure(
                FailureKind::IllegalState,
                "vendor call finished without a result",
            ))))
        );
    }

    #[test]
    fn bridge_preserves_failed_and_cancelled_joins_without_a_payload() {
        for error in [
            VendorError::Cancelled(requested("cancelled")),
            VendorError::Other(super::super::shared::failure(FailureKind::Runtime, "failed")),
        ] {
            let vendor = ManualVendor::default();
            let outcome = error.clone();
            let mut call = Box::pin(call_vendor::<()>(&vendor, super::super::Signal::new(), move |_| {
                Box::pin(async move { Err(outcome) })
            }));
            assert!(call.as_mut().now_or_never().is_none());
            assert_eq!(vendor.poll_body(), Poll::Ready(Err(error.clone())));
            vendor.done.complete(Err(error.clone()));
            assert_eq!(call.as_mut().now_or_never(), Some(Err(error)));
        }
    }
}
