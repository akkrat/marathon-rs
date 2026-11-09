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
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::Notify;

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
    let cell = Arc::new(Mutex::new(None));
    let output = cell.clone();
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
            if let Ok(mut slot) = output.lock() {
                *slot = Some(value);
            }
            Ok(())
        }),
        gate,
    }));
    // Protected join: the vendor task's own result, never the waiter's cancellation.
    task.join().await?;
    let value = cell.lock().ok().and_then(|mut slot| slot.take());
    value.ok_or_else(|| {
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
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let poll = self.body.as_mut().poll(cx);
        if !self.gate.armed.swap(true, Ordering::AcqRel) {
            self.gate.changed.notify_waiters();
        }
        poll
    }
}
