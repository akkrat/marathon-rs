//! Vendor tasks an owner keeps until `join_pending`; Rust-only lifecycle support
//! for the ports of ConnectionFactory.kt and RsyncFileBridge.kt.

use marathon_core::{
    error::{VendorError, VendorResult},
    executor::shared::SharedDeferred,
    runtime::{VendorRuntime, VendorTask, VendorTaskBody},
};
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Poll,
};

struct Retained {
    task: Arc<dyn VendorTask>,
    succeeded: Arc<AtomicBool>,
}

/// D84: `body`'s panic as its error, the one the runtime gives the task's joiner. A retained task that publishes its
/// outcome to a waiter after awaiting its work wraps the work in this, so that a panic is published too and the waiter,
/// which never joins the task, does not wait for ever (U206).
pub async fn panic_as_error<T>(body: impl Future<Output = VendorResult<T>>) -> VendorResult<T> {
    let mut body = std::pin::pin!(body);
    std::future::poll_fn(move |cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body.as_mut().poll(cx))) {
            Ok(poll) => poll,
            Err(payload) => Poll::Ready(Err(VendorError::Other(marathon_core::executor::panic_failure(payload)))),
        }
    })
    .await
}

/// Each spawn prunes tasks whose body already succeeded, so a long run keeps
/// only live handles. Failed tasks stay until joined: their error must still
/// reach `join_pending`.
#[derive(Default)]
pub struct RetainedTasks(Mutex<Vec<Retained>>);

struct PublishOnDrop<T: Send + Sync + 'static>(Arc<SharedDeferred<T>>);
impl<T: Send + Sync + 'static> Drop for PublishOnDrop<T> {
    fn drop(&mut self) {
        self.0
            .cancel(marathon_core::executor::shared::requested("Retained task abandoned"));
    }
}

impl RetainedTasks {
    pub fn spawn(
        &self,
        runtime: &dyn VendorRuntime,
        body: impl Future<Output = VendorResult<()>> + Send + 'static,
    ) -> Arc<dyn VendorTask> {
        self.spawn_owned(runtime, body, false)
    }
    pub fn spawn_interruptible(
        &self,
        runtime: &dyn VendorRuntime,
        body: impl Future<Output = VendorResult<()>> + Send + 'static,
    ) -> Arc<dyn VendorTask> {
        self.spawn_owned(runtime, body, true)
    }
    fn spawn_owned(
        &self,
        runtime: &dyn VendorRuntime,
        body: impl Future<Output = VendorResult<()>> + Send + 'static,
        interruptible: bool,
    ) -> Arc<dyn VendorTask> {
        let succeeded = Arc::new(AtomicBool::new(false));
        let flag = succeeded.clone();
        let run = Box::pin(async move {
            let result = body.await;
            flag.store(result.is_ok(), Ordering::Release);
            result
        });
        let task = if interruptible {
            runtime.spawn_interruptible(run)
        } else {
            runtime.spawn(run)
        };
        let mut tasks = self.0.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|retained| !retained.succeeded.load(Ordering::Acquire));
        tasks.push(Retained {
            task: task.clone(),
            succeeded,
        });
        task
    }
    /// D84: retains `work` and publishes its outcome to `cell`, a panic included, for callers that wait on the cell
    /// rather than join the task (U206).
    pub fn spawn_published<T: Send + Sync + 'static>(
        &self,
        runtime: &dyn VendorRuntime,
        cell: Arc<SharedDeferred<T>>,
        work: impl Future<Output = VendorResult<T>> + Send + 'static,
    ) -> Arc<dyn VendorTask> {
        self.publish(runtime, cell, work, false)
    }
    pub fn spawn_published_interruptible<T: Send + Sync + 'static>(
        &self,
        runtime: &dyn VendorRuntime,
        cell: Arc<SharedDeferred<T>>,
        work: impl Future<Output = VendorResult<T>> + Send + 'static,
    ) -> Arc<dyn VendorTask> {
        self.publish(runtime, cell, work, true)
    }
    fn publish<T: Send + Sync + 'static>(
        &self,
        runtime: &dyn VendorRuntime,
        cell: Arc<SharedDeferred<T>>,
        work: impl Future<Output = VendorResult<T>> + Send + 'static,
        interruptible: bool,
    ) -> Arc<dyn VendorTask> {
        let publish = PublishOnDrop(cell);
        self.spawn_owned(
            runtime,
            async move {
                let result = panic_as_error(work).await;
                publish.0.complete(result);
                Ok(())
            },
            interruptible,
        )
    }
    /// Cancellation-aware owners retain even work that outlives a bounded shutdown.
    pub fn spawn_cancellable(&self, runtime: &dyn VendorRuntime, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let succeeded = Arc::new(AtomicBool::new(false));
        let flag = succeeded.clone();
        let task = runtime.spawn_cancellable(Box::new(move |signal| {
            Box::pin(async move {
                let result = body(signal).await;
                flag.store(result.is_ok(), Ordering::Release);
                result
            })
        }));
        let mut tasks = self.0.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|retained| !retained.succeeded.load(Ordering::Acquire));
        tasks.push(Retained {
            task: task.clone(),
            succeeded,
        });
        task
    }
    pub fn snapshot(&self) -> Vec<Arc<dyn VendorTask>> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| r.task.clone())
            .collect()
    }
    pub fn take(&self) -> Vec<Arc<dyn VendorTask>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
            .into_iter()
            .map(|retained| retained.task)
            .collect()
    }
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
