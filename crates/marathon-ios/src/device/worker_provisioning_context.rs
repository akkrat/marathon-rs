//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/WorkerProvisioningContext.kt

use super::error::{ProvisioningResult, clock_error};
use jcompat::writers::JavaString;
use marathon_apple::{
    bin::{
        AppleBinaryEnvironment,
        xcrun::simctl::model::{SimctlDevice, SimctlListDevicesOutput},
    },
    device::connection_factory::Executor,
    logparser::parser::{DeviceFailureException, DeviceFailureReason},
    model::XcodeVersion,
};
use marathon_config::devices::Transport;
use marathon_core::{error::VendorError, model::DeviceFeature, time::MonotonicClock, vendor::VendorContext};
use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};
use tokio::sync::Mutex as AsyncMutex;

pub const MAX_INVENTORY_AGE_NANOS: i64 = 10_000_000_000;
pub const NANO_TIME: &str = "ios.WorkerProvisioningContext.nanoTime";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerFacts {
    pub env: Vec<(JavaString, JavaString)>,
    pub xcode_version: XcodeVersion,
    pub arch: JavaString,
    pub features: Vec<DeviceFeature>,
    pub supports_transcoding: bool,
    pub shareable: bool,
}
struct Loaded<T> {
    value: Arc<T>,
    executor: Executor,
    started_at: i64,
}
impl<T> Clone for Loaded<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            executor: self.executor.clone(),
            started_at: self.started_at,
        }
    }
}
impl<T> Loaded<T> {
    fn reusable(&self, executor: &Executor) -> Option<Arc<T>> {
        // Kotlin short-circuits on allocation identity, even if that executor disconnected.
        (Arc::ptr_eq(&self.executor, executor)
            || catch_unwind(AssertUnwindSafe(|| self.executor.connected())).unwrap_or(false))
        .then(|| self.value.clone())
    }
}
#[derive(Default)]
struct InventoryState {
    loaded: Option<Loaded<SimctlListDevicesOutput>>,
    published: i64,
    started: i64,
}
/// Make one per transport and planning/setup or reconnect cycle; never cache by host.
pub struct WorkerProvisioningContext {
    pub transport: Transport,
    clock: Arc<dyn MonotonicClock>,
    max_age: i64,
    inventory: Mutex<InventoryState>,
    facts: Mutex<Option<Loaded<WorkerFacts>>>,
    inventory_load: AsyncMutex<()>,
    facts_load: AsyncMutex<()>,
}
impl WorkerProvisioningContext {
    pub fn new(transport: Transport, clock: Arc<dyn MonotonicClock>) -> Self {
        Self::with_max_age(transport, clock, MAX_INVENTORY_AGE_NANOS)
    }
    pub fn with_max_age(transport: Transport, clock: Arc<dyn MonotonicClock>, max_age: i64) -> Self {
        Self {
            transport,
            clock,
            max_age,
            inventory: Mutex::default(),
            facts: Mutex::default(),
            inventory_load: AsyncMutex::new(()),
            facts_load: AsyncMutex::new(()),
        }
    }
    pub async fn inventory(
        &self,
        bin: &AppleBinaryEnvironment,
        caller: &VendorContext,
    ) -> ProvisioningResult<Arc<SimctlListDevicesOutput>> {
        if let Some(value) = self.reusable_inventory(&bin.command_executor)? {
            return Ok(value);
        }
        let _load = tokio::select! { biased;
            guard = self.inventory_load.lock() => guard,
            reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
        };
        if let Some(value) = self.reusable_inventory(&bin.command_executor)? {
            return Ok(value);
        }
        self.refresh(bin, caller).await
    }
    pub async fn refresh(
        &self,
        bin: &AppleBinaryEnvironment,
        caller: &VendorContext,
    ) -> ProvisioningResult<Arc<SimctlListDevicesOutput>> {
        self.refresh_with(bin.command_executor.clone(), || async {
            bin.xcrun.simctl.device.list(caller).await.map_err(Into::into)
        })
        .await
    }
    fn reusable_inventory(&self, executor: &Executor) -> ProvisioningResult<Option<Arc<SimctlListDevicesOutput>>> {
        let loaded = self.inventory.lock().unwrap_or_else(|e| e.into_inner()).loaded.clone();
        let Some(loaded) = loaded else {
            return Ok(None);
        };
        let now = self.clock.nanos(NANO_TIME).map_err(clock_error)?;
        if now.wrapping_sub(loaded.started_at) > self.max_age {
            return Ok(None);
        }
        Ok(loaded.reusable(executor))
    }
    pub async fn inventory_with<F, Fut>(
        &self,
        executor: Executor,
        caller: &VendorContext,
        load: F,
    ) -> ProvisioningResult<Arc<SimctlListDevicesOutput>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ProvisioningResult<SimctlListDevicesOutput>>,
    {
        if let Some(value) = self.reusable_inventory(&executor)? {
            return Ok(value);
        }
        let _load = tokio::select! { biased;
            guard = self.inventory_load.lock() => guard,
            reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
        };
        if let Some(value) = self.reusable_inventory(&executor)? {
            return Ok(value);
        }
        self.refresh_with(executor, load).await
    }
    /// Refresh bypasses inventory_load: a slow request cannot hold cached readers or another refresh.
    pub async fn refresh_with<F, Fut>(
        &self,
        executor: Executor,
        load: F,
    ) -> ProvisioningResult<Arc<SimctlListDevicesOutput>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ProvisioningResult<SimctlListDevicesOutput>>,
    {
        let (request, started_at) = {
            let mut state = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
            state.started = state.started.wrapping_add(1);
            (state.started, self.clock.nanos(NANO_TIME).map_err(clock_error)?)
        };
        let value = Arc::new(load().await?);
        let mut state = self.inventory.lock().unwrap_or_else(|e| e.into_inner());
        if request > state.published {
            state.loaded = Some(Loaded {
                value: value.clone(),
                executor,
                started_at,
            });
            state.published = request;
        }
        Ok(value)
    }
    pub async fn simulator(
        &self,
        udid: &str,
        bin: &AppleBinaryEnvironment,
        fresh: bool,
        caller: &VendorContext,
    ) -> ProvisioningResult<SimctlDevice> {
        let inventory = if fresh {
            self.refresh(bin, caller).await?
        } else {
            self.inventory(bin, caller).await?
        };
        let invalid =
            |message| DeviceFailureException::new(DeviceFailureReason::InvalidSimulatorIdentifier, Some(message), None);
        let id = self.transport.id().to_string_lossy();
        let simulator = inventory
            .devices
            .devices
            .iter()
            .find(|d| d.udid == udid)
            .ok_or_else(|| {
                marathon_apple::device::connection_factory::ConnectionError::Device(invalid(format!(
                    "udid {udid} is missing at {id}"
                )))
            })?;
        if simulator.is_available == Some(false) {
            return Err(
                marathon_apple::device::connection_factory::ConnectionError::Device(invalid(format!(
                    "udid {udid} is not available at {id}"
                )))
                .into(),
            );
        }
        Ok(simulator.clone())
    }
    fn reusable_facts(&self, executor: &Executor) -> Option<Arc<WorkerFacts>> {
        let loaded = self.facts.lock().unwrap_or_else(|e| e.into_inner()).clone();
        loaded.and_then(|loaded| loaded.reusable(executor))
    }
    pub async fn worker_facts<F, Fut>(
        &self,
        executor: Executor,
        caller: &VendorContext,
        load: F,
    ) -> ProvisioningResult<Arc<WorkerFacts>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ProvisioningResult<WorkerFacts>>,
    {
        if let Some(value) = self.reusable_facts(&executor) {
            return Ok(value);
        }
        let _load = tokio::select! { biased;
            guard = self.facts_load.lock() => guard,
            reason = caller.cancellation.cancelled() => return Err(VendorError::Cancelled(reason).into()),
        };
        if let Some(value) = self.reusable_facts(&executor) {
            return Ok(value);
        }
        let value = Arc::new(load().await?);
        if value.shareable {
            let started_at = self.clock.nanos(NANO_TIME).map_err(clock_error)?;
            *self.facts.lock().unwrap_or_else(|e| e.into_inner()) = Some(Loaded {
                value: value.clone(),
                executor,
                started_at,
            });
        }
        Ok(value)
    }
}
