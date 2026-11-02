//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorFactory.kt

use super::{
    error::{ProvisioningFuture, ProvisioningResult},
    timing::ProvisioningTiming,
    worker_provisioning_context::WorkerProvisioningContext,
};
use marathon_apple::{
    bin::{AppleBinaryEnvironment, xcrun::simctl::model::SimctlDevice},
    cmd::shared_transfers::SharedTransfers,
    device::connection_factory::{Bridge, Executor},
    model::Sdk,
};
use marathon_config::devices::Transport;
use marathon_core::{
    analytics::Track,
    runtime::VendorFuture,
    vendor::{DeviceRef, VendorContext},
};
use std::sync::Arc;

pub type BinaryFactory = Arc<dyn Fn(Executor) -> Arc<AppleBinaryEnvironment> + Send + Sync>;
/// 5.2 implements this with its concrete Arc; no setup method or cast is added to core Device.
pub trait SimulatorSetup: Send + Sync {
    fn setup<'a>(
        &'a self,
        record: &'a SimctlDevice,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> VendorFuture<'a, ()>;
}
/// Carries the concrete 5.2 Arc unchanged through factory and provisioner to provider registration.
/// H exposes admission/retire/transport/serial/disposed/connected without core Device downcasts.
#[derive(Clone)]
pub struct ConstructedSimulator<H> {
    pub handle: H,
    pub device: DeviceRef,
    pub setup: Arc<dyn SimulatorSetup>,
}
impl<H> std::fmt::Debug for ConstructedSimulator<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConstructedSimulator")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}
pub struct SimulatorConstruction {
    pub transport: Transport,
    pub executor: Executor,
    pub bridge: Bridge,
    pub bin: Arc<AppleBinaryEnvironment>,
    pub record: SimctlDevice,
    pub sdk: Sdk,
    pub shared_transfers: Arc<SharedTransfers>,
}
/// The adapter captures run-owned bundle/artifact/output/config services; constructing runs no setup.
pub trait SimulatorConstructor: Send + Sync {
    type Handle: Clone + Send + Sync;
    fn construct(&self, input: SimulatorConstruction) -> ProvisioningResult<ConstructedSimulator<Self::Handle>>;
}
pub trait SimulatorCreating: Send + Sync {
    type Handle: Clone + Send + Sync;
    fn create<'a>(
        &'a self,
        transport: Transport,
        executor: Executor,
        bridge: Bridge,
        udid: String,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> ProvisioningFuture<'a, ConstructedSimulator<Self::Handle>>;
}
pub struct SimulatorFactory<H> {
    binaries: BinaryFactory,
    constructor: Arc<dyn SimulatorConstructor<Handle = H>>,
    shared_transfers: Arc<SharedTransfers>,
    track: Track,
    timing: ProvisioningTiming,
}
impl<H> SimulatorFactory<H> {
    pub fn new(
        binaries: BinaryFactory,
        constructor: Arc<dyn SimulatorConstructor<Handle = H>>,
        shared_transfers: Arc<SharedTransfers>,
        track: Track,
        timing: ProvisioningTiming,
    ) -> Self {
        Self {
            binaries,
            constructor,
            shared_transfers,
            track,
            timing,
        }
    }
}
pub fn simulator_sdk(device_type: Option<&str>) -> Sdk {
    let id = device_type
        .unwrap_or("")
        .strip_prefix("com.apple.CoreSimulator.SimDeviceType.")
        .unwrap_or(device_type.unwrap_or(""));
    if id.starts_with("Apple-TV") {
        Sdk::TvSimulator
    } else if id.starts_with("Apple-Watch") {
        Sdk::WatchSimulator
    } else if id.starts_with("Apple-Vision") {
        Sdk::VisionSimulator
    } else {
        Sdk::IphoneSimulator
    }
}
impl<H: Clone + Send + Sync> SimulatorCreating for SimulatorFactory<H> {
    type Handle = H;
    fn create<'a>(
        &'a self,
        transport: Transport,
        executor: Executor,
        bridge: Bridge,
        udid: String,
        cycle: Arc<WorkerProvisioningContext>,
        caller: VendorContext,
    ) -> ProvisioningFuture<'a, ConstructedSimulator<Self::Handle>> {
        Box::pin(async move {
            let started = self.timing.elapsed()?;
            let bin = (self.binaries)(executor.clone());
            let record = cycle.simulator(&udid, &bin, false, &caller).await?;
            let sdk = simulator_sdk(record.device_type_identifier.as_deref());
            let built = self.constructor.construct(SimulatorConstruction {
                transport,
                executor,
                bridge,
                bin,
                record: record.clone(),
                sdk,
                shared_transfers: self.shared_transfers.clone(),
            })?;
            let device = &built.device;
            let result: ProvisioningResult<()> = async {
                self.track
                    .track_provider_device_preparing(
                        || device.0.info().serial_number,
                        built.setup.setup(&record, cycle, caller),
                    )
                    .await?;
                self.timing.info(|| {
                    Ok(format!(
                        "Simulator {} set up in {} ms",
                        device.0.info().serial_number.to_string_lossy(),
                        self.timing.elapsed()?.wrapping_sub(started)
                    ))
                })?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => Ok(built),
                Err(mut original) => {
                    // dispose has no caller cancellation context and is awaited through completion.
                    if let Err(cleanup) = device.0.dispose().await {
                        original.suppressed.push(cleanup.into());
                    }
                    Err(original)
                }
            }
        })
    }
}
