//! Port of core/src/main/kotlin/com/malinskiy/marathon/device/Device.kt
//! and device/DeviceProvider.kt, execution/TestParser.kt, Vendor.kt.

use crate::{
    error::VendorResult,
    model::{BatchRef, DeviceInfo, DevicePoolId, Test, TestBatchResults},
    runtime::{CancellationSignal, Deferred, EventChannel, VendorClock, VendorFuture},
    time::ReplayHooks,
};
use marathon_config::Configuration;
use marathon_logging::{Component, LogSettings};
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
    fn abi(&self) -> String;
    fn worker_id(&self) -> Option<String>;
    /// Handle from the per-run jcompat LogSettings registry.
    fn component(&self) -> &Component;
    fn prepare<'a>(&'a self, configuration: Arc<Configuration>, context: VendorContext) -> VendorFuture<'a, ()>;
    /// D85: the device actor has exhausted its preparation retries. Must not perform I/O or block on core.
    fn prepare_exhausted(&self) {}
    /// Returning and completing result are independent. Empty/no result and a
    /// result followed by a failure are both valid. Never auto-complete on return.
    fn execute<'a>(
        &'a self,
        configuration: Arc<Configuration>,
        pool: DevicePoolId,
        batch: BatchRef,
        result: Arc<Deferred<TestBatchResults>>,
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
    /// D84: how long a cancelled call of this device can take to end, by the vendor's own bound for cleaning up after a
    /// cancel; `None` when there is no overall bound and the call ends once each request it waits on has timed out. A
    /// run stopping after an internal failure waits this long (plus a margin) for the device before it finishes anyway.
    fn cancellation_cleanup_millis(&self) -> Option<i64> {
        None
    }
    /// D84: ends, or waits for, the work this device keeps running after a cancelled call returned to its caller (an
    /// Apple preparation runs on in a retained task), so that a stopping run can wait for it before the provider
    /// terminates. Bounded by the run with `cancellation_cleanup_millis`, which must cover it.
    fn join_cancelled_work(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// D70: this device is leaving the pool while it holds `batch`, whose result is not in. True if the vendor has
    /// started none of it and from now on will not start any: the batch then goes back to the queue with no attempt
    /// charged, as a batch that never reached the device does. A vendor that has started part of it may instead complete
    /// `result`, the one `execute` was given, with what it has, and say false. False keeps the batch's usual accounting.
    /// Runs on the core thread: no I/O, no blocking.
    fn withdraw_unstarted(&self, batch: &BatchRef) -> bool {
        let _ = batch;
        false
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
    fn borrow_untried(&self, tried: Vec<String>, context: VendorContext) -> VendorFuture<'_, (DeviceRef, String)> {
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
    fn id(&self) -> &str;
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
    fn configure(&self, debug: bool, logging: Arc<Mutex<LogSettings>>) -> VendorFuture<'_, ()>;
}
/// Koin module replacement. Construction itself must not initialize the provider
/// or query analytics; Marathon owns those lifecycle stages.
pub struct VendorServices {
    pub provider: Arc<dyn DeviceProvider>,
    pub parser: Option<TestParser>,
    pub bundle_identifier: Option<Arc<dyn TestBundleIdentifier>>,
    pub log_configurator: Option<Arc<dyn LogConfigurator>>,
}
