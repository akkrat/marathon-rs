//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolMessage.kt
use crate::{
    model::{BatchRef, DeviceInfo, TestBatchResults},
    vendor::DeviceRef,
};

/// Data-class equality: devices by identity, batches and results structurally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DevicePoolMessage {
    FromScheduler(FromScheduler),
    FromDevice(FromDevice),
    FromQueue(FromQueue),
    /// D48: the wait with this number for a device to return has run out.
    ReturnWaitOver(u64),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromScheduler {
    AddDevice(DeviceRef),
    RemoveDevice(DeviceRef),
    Terminate,
    /// D84: answered once every message sent to the pool before it has been handled.
    Drain(DrainBarrier),
}
/// D84: the pool's answer to `Drain`, a cell the Scheduler waits on. Equal by identity.
#[derive(Clone)]
pub struct DrainBarrier(pub std::sync::Arc<crate::executor::shared::SharedDeferred<()>>);
impl std::fmt::Debug for DrainBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DrainBarrier")
    }
}
impl PartialEq for DrainBarrier {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for DrainBarrier {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromDevice {
    IsReady(DeviceRef),
    CompletedTestBatch(DeviceRef, TestBatchResults),
    ReturnTestBatch(DeviceRef, BatchRef, String),
    /// D70: the device left before it started any of the batch, which goes back with no attempt charged.
    RejectTestBatch(DeviceRef, BatchRef),
    Terminated(DeviceRef),
}
impl FromDevice {
    pub fn device(&self) -> &DeviceRef {
        match self {
            Self::IsReady(d)
            | Self::CompletedTestBatch(d, _)
            | Self::ReturnTestBatch(d, _, _)
            | Self::RejectTestBatch(d, _)
            | Self::Terminated(d) => d,
        }
    }
}
#[derive(Clone, Debug, Eq)]
pub enum FromQueue {
    /// Workers of the attempts that queued the retries waiting in the queue, in
    /// the queue's LinkedHashSet order without duplicates.
    Notify {
        retry_workers: Vec<String>,
    },
    NoBatch(DeviceInfo),
    Terminated,
    ExecuteBatch(DeviceInfo, BatchRef),
    OutcomeProcessed(DeviceInfo),
}
/// `Notify.retryWorkers` is a Kotlin `Set`: equal whatever the order.
impl PartialEq for FromQueue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Notify { retry_workers: a }, Self::Notify { retry_workers: b }) => {
                a.len() == b.len() && a.iter().all(|worker| b.contains(worker))
            }
            (Self::NoBatch(a), Self::NoBatch(b)) | (Self::OutcomeProcessed(a), Self::OutcomeProcessed(b)) => a == b,
            (Self::Terminated, Self::Terminated) => true,
            (Self::ExecuteBatch(a, x), Self::ExecuteBatch(b, y)) => a == b && x == y,
            _ => false,
        }
    }
}
