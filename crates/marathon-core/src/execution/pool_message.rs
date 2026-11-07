//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/DevicePoolMessage.kt
use crate::{
    model::{BatchRef, DeviceInfo, TestBatchResults},
    vendor::DeviceRef,
};
use jcompat::writers::JavaString;

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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromDevice {
    IsReady(DeviceRef),
    CompletedTestBatch(DeviceRef, TestBatchResults),
    ReturnTestBatch(DeviceRef, BatchRef, JavaString),
    Terminated(DeviceRef),
}
impl FromDevice {
    pub fn device(&self) -> &DeviceRef {
        match self {
            Self::IsReady(d)
            | Self::CompletedTestBatch(d, _)
            | Self::ReturnTestBatch(d, _, _)
            | Self::Terminated(d) => d,
        }
    }
}
#[derive(Clone, Debug, Eq)]
pub enum FromQueue {
    /// Workers of the attempts that queued the retries waiting in the queue, in
    /// the queue's LinkedHashSet order without duplicates.
    Notify {
        retry_workers: Vec<JavaString>,
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
