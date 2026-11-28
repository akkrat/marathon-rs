//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/device/DeviceActor.kt,
//! DeviceAction.kt, DeviceEvent.kt and DeviceState.kt.
//!
//! These are the actor's own events; the provider's connect/disconnect events
//! are `crate::vendor::DeviceEvent`.
mod actor;
mod state;
pub use actor::{DeviceActor, DeviceActorRef};
pub use state::{DeviceAction, DeviceEvent, DeviceState, Running};

use crate::{
    error::VendorError,
    executor::{CoreExecutor, Signal, bridge::call_vendor},
    runtime::JobRef,
    vendor::DeviceRef,
};

pub(super) struct StoppingDevice {
    pub job: JobRef,
    pub device: DeviceRef,
    cleanup: Option<JobRef>,
}
impl StoppingDevice {
    /// Start vendor cleanup before the caller cancels the actor (D92, D93).
    pub fn begin(core: &CoreExecutor, job: JobRef, device: DeviceRef, pool_stop: bool) -> Self {
        if pool_stop {
            device.0.seal_for_pool_stop();
        } else {
            device.0.seal_for_stop();
        }
        let cleanup = device.0.begin_stop().map(|cleanup| {
            let vendor = core.vendor();
            core.spawn_cleanup(Box::new(move |_| {
                Box::pin(async move {
                    call_vendor(vendor.as_ref(), Signal::new(), move |_| cleanup)
                        .await
                        .map_err(|err| match err {
                            VendorError::Cancelled(reason) => reason.into(),
                            other => other.failure().clone().into(),
                        })
                })
            }))
        });
        Self { job, device, cleanup }
    }

    /// The cancelled call, then D92 cleanup or the vendor's retained work receipt (D93).
    pub async fn join(&self, core: &CoreExecutor) {
        let _ = self.job.join().await;
        if let Some(cleanup) = &self.cleanup {
            let _ = cleanup.join().await;
        } else {
            let held = self.device.0.clone();
            let _ = call_vendor(core.vendor().as_ref(), Signal::new(), move |_| {
                Box::pin(async move { held.join_cancelled_work().await })
            })
            .await;
        }
    }
}
