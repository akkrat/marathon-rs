//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
#![allow(dead_code)]
use crate::{simulator_support, support};
use marathon_apple::cmd::{FileBridge, SessionScope, failure};
use marathon_core::{error::*, model::*, runtime::*};
use marathon_error::{FailureKind, InjectedKind};
use marathon_ios::{apple_simulator_device::*, listener::ListenerContext};
use std::path::Path;
use std::sync::Arc;
#[derive(Default)]
pub struct Runtime;
struct Task {
    signal: Arc<SessionScope>,
    inner: Arc<dyn VendorTask>,
}
impl VendorTask for Task {
    fn cancel(&self, reason: Cancellation) {
        self.signal.cancel(reason);
    }
    fn join(&self) -> VendorFuture<'_, ()> {
        self.inner.join()
    }
}
impl VendorRuntime for Runtime {
    fn spawn_cancellable(&self, body: VendorTaskBody) -> Arc<dyn VendorTask> {
        let signal = Arc::new(SessionScope::default());
        Arc::new(Task {
            inner: support::Runtime.spawn(body(signal.clone())),
            signal,
        })
    }
    fn spawn(&self, body: VendorFuture<'static, ()>) -> Arc<dyn VendorTask> {
        support::Runtime.spawn(body)
    }
    fn spawn_blocking(&self, body: Box<dyn FnOnce() -> VendorResult<()> + Send>) -> Arc<dyn VendorTask> {
        support::Runtime.spawn_blocking(body)
    }
}
pub fn harness(
    edit: impl FnOnce(&mut marathon_config::vendor::IosConfiguration),
) -> simulator_support::TestResult<simulator_support::Harness> {
    let mut h = simulator_support::Harness::new(edit)?;
    h.services
        .logging
        .registry
        .lock()
        .unwrap()
        .configure(true, marathon_logging::Vendor::Apple { compact_output: false });
    h.services.runtime = Arc::new(Runtime);
    h.services.listeners = Arc::new(marathon_ios::listener::ProductionSimulatorListenerFactory);
    Ok(h)
}
pub fn context(h: &simulator_support::Harness, d: Arc<AppleSimulatorDevice>) -> ListenerContext {
    ListenerContext {
        device: d,
        pool: DevicePoolId { name: "pool".into() },
        batch: Arc::new(TestBatch {
            id: "batch".into(),
            tests: simulator_support::tests(),
        }),
        context: h.services.context.clone(),
    }
}
#[derive(Default)]
pub struct Bridge {
    pub pulls: std::sync::Mutex<Vec<String>>,
    pub mode: std::sync::atomic::AtomicUsize,
}
impl FileBridge for Bridge {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.pulls.lock().unwrap().push(src.into());
            match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
                1 => return Ok(false),
                2 => {
                    return Err(VendorError::Transfer(failure(
                        FailureKind::Injected(InjectedKind::TransferException),
                        "transfer",
                    )));
                }
                3 => return Err(VendorError::Other(failure(FailureKind::IllegalState, "other"))),
                _ => (),
            }
            std::fs::write(dst, b"media").map_err(|e| VendorError::Other(failure(FailureKind::Io, e.to_string())))?;
            Ok(true)
        })
    }
}
pub async fn device(
    h: &simulator_support::Harness,
    bridge: Arc<Bridge>,
) -> simulator_support::TestResult<Arc<AppleSimulatorDevice>> {
    h.device_on_bridge(
        h.worker.clone(),
        "a",
        Arc::new(
            marathon_ios::device::worker_provisioning_context::WorkerProvisioningContext::new(
                marathon_config::devices::Transport::Local,
                h.clock.clone(),
            ),
        ),
        bridge,
    )
    .await
}
