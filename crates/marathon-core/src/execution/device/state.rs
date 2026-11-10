//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceState.kt,
//! DeviceEvent.kt and DeviceAction.kt.
use crate::{
    executor::SharedDeferred,
    model::{BatchRef, TestBatchResults},
};
use std::{fmt, sync::Arc};

/// The batch being executed and its parentless result cell (`DeviceActor.kt:58–60`).
#[derive(Clone)]
pub struct Running {
    pub test_batch: BatchRef,
    pub result: Arc<SharedDeferred<TestBatchResults>>,
}
#[derive(Clone)]
pub enum DeviceState {
    Connected,
    Ready,
    Initializing,
    Running(Running),
    Terminated,
}
/// `GetDeviceState` is omitted: nothing sends it at e47352f2 (S4).
#[derive(Clone)]
pub enum DeviceEvent {
    Execute(BatchRef),
    Complete,
    Initialize,
    Terminate,
}
pub enum DeviceAction {
    Initialize,
    Terminate(Option<Running>),
    ExecuteBatch(BatchRef, Arc<SharedDeferred<TestBatchResults>>),
    NotifyIsReady(Option<Running>),
}

// Used by invalid-transition diagnostics; the batch id identifies the work.
impl fmt::Display for DeviceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connected => f.write_str("connected"),
            Self::Ready => f.write_str("ready"),
            Self::Initializing => f.write_str("initializing"),
            Self::Running(r) => write!(f, "running batch={}", r.test_batch.id.to_owned()),
            Self::Terminated => f.write_str("terminated"),
        }
    }
}
impl fmt::Display for DeviceEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execute(batch) => write!(f, "execute batch={}", batch.id.to_owned()),
            Self::Complete => f.write_str("complete"),
            Self::Initialize => f.write_str("initialize"),
            Self::Terminate => f.write_str("terminate"),
        }
    }
}
