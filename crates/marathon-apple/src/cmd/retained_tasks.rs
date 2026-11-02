//! Vendor tasks an owner keeps until `join_pending`; Rust-only lifecycle support
//! for the ports of ConnectionFactory.kt and RsyncFileBridge.kt.

use marathon_core::{
    error::VendorResult,
    runtime::{VendorRuntime, VendorTask, VendorTaskBody},
};
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Retained {
    task: Arc<dyn VendorTask>,
    succeeded: Arc<AtomicBool>,
}

/// Each spawn prunes tasks whose body already succeeded, so a long run keeps
/// only live handles. Failed tasks stay until joined: their error must still
/// reach `join_pending`.
#[derive(Default)]
pub struct RetainedTasks(Mutex<Vec<Retained>>);

impl RetainedTasks {
    pub fn spawn(
        &self,
        runtime: &dyn VendorRuntime,
        body: impl Future<Output = VendorResult<()>> + Send + 'static,
    ) -> Arc<dyn VendorTask> {
        let succeeded = Arc::new(AtomicBool::new(false));
        let flag = succeeded.clone();
        let task = runtime.spawn(Box::pin(async move {
            let result = body.await;
            flag.store(result.is_ok(), Ordering::Release);
            result
        }));
        let mut tasks = self.0.lock().unwrap_or_else(|e| e.into_inner());
        tasks.retain(|retained| !retained.succeeded.load(Ordering::Acquire));
        tasks.push(Retained {
            task: task.clone(),
            succeeded,
        });
        task
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
