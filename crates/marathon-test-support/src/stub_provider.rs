//! Port of vendor/vendor-test/src/main/kotlin/com/malinskiy/marathon/test/StubDeviceProvider.kt
//!
//! One shared consuming queue. Kotlin's `providingLogic` coroutine is replaced by the
//! replay driver delivering connect/disconnect events into that queue at scripted times
//! (`ControlledReplay::event`), so no task is launched from the synchronous `subscribe`.
use crate::lock;

use marathon_core::{
    error::*,
    executor::shared::SharedChannel,
    runtime::*,
    vendor::{DeviceEvent, DeviceProvider, DeviceRef, VendorContext},
};
use std::sync::{Arc, Mutex};

pub type ProviderHook = Arc<dyn Fn(VendorContext) -> VendorFuture<'static, ()> + Send + Sync>;
pub type TerminateHook = Arc<dyn Fn() -> VendorFuture<'static, ()> + Send + Sync>;

pub struct StubDeviceProvider {
    channel: Arc<SharedChannel<DeviceEvent>>,
    /// `lateinit var borrowingDevice`.
    pub borrowing_device: Mutex<Option<DeviceRef>>,
    /// `initialize() {}` unless a witness supplies its own body.
    pub on_initialize: Mutex<Option<ProviderHook>>,
    /// Runs before the queue is closed (finish-slot witnesses).
    pub on_terminate: Mutex<Option<TerminateHook>>,
    /// Replaces the borrowed device with this failure (remote-parse retry witnesses).
    pub borrow_error: Mutex<Option<VendorError>>,
    /// D53 witnesses: when set, a remote parse borrows from these instead of `borrowing_device`, taking the first whose
    /// serial number no earlier attempt failed on, or the first of all.
    pub borrowable: Mutex<Vec<DeviceRef>>,
    pub on_subscribe: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Virtual time `borrow` takes before returning the device.
    pub borrow_delay_millis: Mutex<i64>,
    pub calls: Mutex<Vec<&'static str>>,
}
impl StubDeviceProvider {
    pub fn new(channel: Arc<SharedChannel<DeviceEvent>>) -> Arc<Self> {
        Arc::new(Self {
            channel,
            borrowing_device: Mutex::new(None),
            on_initialize: Mutex::new(None),
            on_terminate: Mutex::new(None),
            borrow_error: Mutex::new(None),
            borrowable: Mutex::default(),
            on_subscribe: Mutex::new(None),
            borrow_delay_millis: Mutex::new(0),
            calls: Mutex::default(),
        })
    }
    pub fn channel(&self) -> Arc<SharedChannel<DeviceEvent>> {
        self.channel.clone()
    }
    /// `channel.send(event)` from the providing logic.
    pub fn send(&self, event: DeviceEvent) -> bool {
        self.channel.try_send(event).is_ok()
    }
}
impl DeviceProvider for StubDeviceProvider {
    fn initialize(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        lock(&self.calls).push("initialize");
        let hook = lock(&self.on_initialize).clone();
        Box::pin(async move {
            match hook {
                Some(hook) => hook(context).await,
                None => Ok(()),
            }
        })
    }
    fn borrow(&self, context: VendorContext) -> VendorFuture<'_, DeviceRef> {
        lock(&self.calls).push("borrow");
        let device = lock(&self.borrowing_device).clone();
        let delay = *lock(&self.borrow_delay_millis);
        let error = lock(&self.borrow_error).clone();
        Box::pin(async move {
            if delay > 0 {
                context.clock.delay(delay, context.cancellation.clone()).await?;
            }
            if let Some(error) = error {
                return Err(error);
            }
            device.ok_or_else(|| {
                VendorError::Other(marathon_core::executor::shared::failure(
                    "kotlin.UninitializedPropertyAccessException",
                    "lateinit property borrowingDevice has not been initialized",
                ))
            })
        })
    }
    fn borrow_untried(&self, tried: Vec<String>, context: VendorContext) -> VendorFuture<'_, (DeviceRef, String)> {
        let devices = lock(&self.borrowable).clone();
        if devices.is_empty() {
            let borrowed = self.borrow(context);
            return Box::pin(async move {
                let device = borrowed.await?;
                let key = device.0.info().serial_number;
                Ok((device, key))
            });
        }
        lock(&self.calls).push("borrow");
        let device = devices
            .iter()
            .find(|d| !tried.contains(&d.0.info().serial_number))
            .unwrap_or(&devices[0])
            .clone();
        Box::pin(async move {
            let key = device.0.info().serial_number;
            Ok((device, key))
        })
    }
    fn terminate(&self) -> VendorFuture<'_, ()> {
        lock(&self.calls).push("terminate");
        let hook = lock(&self.on_terminate).clone();
        Box::pin(async move {
            if let Some(hook) = hook {
                hook().await?;
            }
            // `channel.close()`: graceful, the buffer drains.
            self.channel.close(None);
            Ok(())
        })
    }
    fn subscribe(&self) -> Arc<dyn EventChannel<DeviceEvent>> {
        lock(&self.calls).push("subscribe");
        if let Some(hook) = lock(&self.on_subscribe).clone() {
            hook();
        }
        self.channel.clone()
    }
}
