//! Port of core/src/main/kotlin/com/malinskiy/marathon/device/Device.kt
//! and device/DeviceProvider.kt, execution/TestParser.kt, Vendor.kt.

use crate::{
    error::VendorResult,
    model::{BatchRef, DeviceInfo, DevicePoolId, Test, TestBatchResults},
    runtime::{CancellationSignal, Deferred, EventChannel, VendorClock, VendorFuture},
    time::ReplayHooks,
};
use jcompat::{
    logging::{Logger, MarathonLogging},
    writers::JavaString,
};
use marathon_config::Configuration;
use std::sync::{Arc, Mutex};

/// One stable allocation per Kotlin Device object; serial reuse is not identity.
/// Cloning this wrapper preserves identity. A replacement gets a new allocation.
#[derive(Clone)]
pub struct DeviceRef(pub Arc<dyn Device>);
impl PartialEq for DeviceRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for DeviceRef {}
impl std::fmt::Debug for DeviceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DeviceRef").field(&self.0.info().serial_number).finish()
    }
}

/// Owned by an execution job, not a global runtime. Vendors observe cancellation
/// cooperatively and run required cleanup before their future completes.
/// SSH implementations and fake vendors use docs/design/fake-worker.md for
/// transport evidence and shutdown.
#[derive(Clone)]
pub struct VendorContext {
    pub cancellation: Arc<dyn CancellationSignal>,
    pub clock: Arc<dyn VendorClock>,
    pub hooks: Arc<dyn ReplayHooks>,
}
pub trait Device: Send + Sync {
    /// Fresh snapshot, including health/features that may change after setup.
    /// Must not perform I/O while called on core.
    fn info(&self) -> DeviceInfo;
    fn abi(&self) -> JavaString;
    fn worker_id(&self) -> Option<JavaString>;
    /// Handle from the per-run jcompat MarathonLogging registry.
    fn logger(&self) -> &Logger;
    fn prepare<'a>(&'a self, configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()>;
    /// Returning and completing result are independent. Empty/no result and a
    /// result followed by a failure are both valid. Never auto-complete on return.
    fn execute<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<dyn Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()>;
    /// Kotlin dispose is synchronous; this async adapter awaits off-core work at
    /// the same call site. Drop must not replace it or silently start it.
    /// Idempotent and safe for concurrent callers: a second caller joins the
    /// disposal already in progress (AppleSimulatorDevice.kt:576–610).
    fn dispose(&self) -> VendorFuture<'_, ()>;
    /// D48: how much of an emptied pool's wait this device has used up by leaving: what the vendor already waited for
    /// it, so that the two waits don't add up, or `i64::MAX` if no path can bring it back. Must not perform I/O.
    fn return_wait_used_millis(&self) -> i64 {
        0
    }
}
#[derive(Clone)]
pub enum DeviceEvent {
    Connected(DeviceRef),
    Disconnected(DeviceRef),
}
pub trait DeviceProvider: Send + Sync {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()>;
    /// Before consuming subscribe's channel. No invented release/lease protocol.
    fn borrow(&self, context: VendorContext) -> VendorFuture<'_, DeviceRef>;
    /// D53: `borrow` for one attempt of remote parsing, which passes the keys of the devices its earlier attempts failed
    /// on. Picks among the available devices whose key is not in `tried`, as `borrow` picks among all, and among all once
    /// every one has been tried; it never waits for an untried device. Returns the device with its key: the one the
    /// provider borrows by, so that a device that comes back as a new object keeps it. With `tried` empty this is
    /// `borrow`. The default ignores `tried` and names a device by its serial number.
    fn borrow_untried(
        &self,
        tried: Vec<JavaString>,
        context: VendorContext,
    ) -> VendorFuture<'_, (DeviceRef, JavaString)> {
        let _ = tried;
        Box::pin(async move {
            let device = self.borrow(context).await?;
            let key = device.0.info().serial_number;
            Ok((device, key))
        })
    }
    fn terminate(&self) -> VendorFuture<'_, ()>;
    /// Repeated calls return handles to the same consuming queue, not broadcasts.
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>>;
}
pub trait LocalTestParser: Send + Sync {
    fn extract(&self, context: VendorContext) -> VendorFuture<'_, Vec<Test>>;
}
pub trait RemoteTestParser: Send + Sync {
    fn extract(&self, device: DeviceRef, context: VendorContext) -> VendorFuture<'_, Vec<Test>>;
}
/// Exhaustive local/remote split replaces Kotlin's marker + runtime type check.
#[derive(Clone)]
pub enum TestParser {
    Local(Arc<dyn LocalTestParser>),
    Remote(Arc<dyn RemoteTestParser>),
}
/// AppleTestBundle, AndroidTestBundle and StubTestBundle are ordinary Kotlin
/// classes: equality is reference identity, even when their id strings match.
pub trait TestBundle: Send + Sync {
    fn id(&self) -> &JavaString;
}
#[derive(Clone)]
pub struct BundleRef(pub Arc<dyn TestBundle>);
impl PartialEq for BundleRef {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for BundleRef {}
pub trait TestBundleIdentifier: Send + Sync {
    fn identify(&self, test: &Test) -> VendorResult<Option<BundleRef>>;
}
pub trait LogConfigurator: Send + Sync {
    /// Set registry.debug first (Marathon.kt:67), then configure vendor appenders.
    /// Registry mutation runs off core; never hold its lock across await.
    fn configure(&self, debug: bool, logging: Arc<Mutex<MarathonLogging>>) -> VendorFuture<'_, ()>;
}
/// Koin module replacement. Construction itself must not initialize the provider
/// or query analytics; Marathon owns those lifecycle stages.
pub struct VendorServices {
    pub provider: Arc<dyn DeviceProvider>,
    pub parser: Option<TestParser>,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    pub log_configurator: Option<Arc<dyn LogConfigurator>>,
}
