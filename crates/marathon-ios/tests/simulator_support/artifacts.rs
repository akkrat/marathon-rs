//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/listener/ResultBundleRunListenerTest.kt
#![allow(dead_code)]
use super::*;
use marathon_apple::{
    cmd::{FileBridge, failure},
    listener::AppleTestRunListener,
};
use marathon_core::{
    artifacts::AttachmentProviderDelegate,
    error::{VendorError, VendorResult},
    executor::shared::SharedDeferred,
    model::{Attachment, AttachmentType, BatchRef, DevicePoolId, Test},
    runtime::{Deferred, VendorFuture},
    vendor::VendorContext,
};
use marathon_ios::apple_simulator_device::{SimulatorListenerFactory, SimulatorListeners};
use std::path::Path;
#[derive(Default)]
pub struct BundleBridge {
    pub gate: Mutex<Option<Arc<SharedDeferred<()>>>>,
    pub entered: Arc<SharedDeferred<()>>,
    pub mode: AtomicUsize,
    pub pulls: AtomicUsize,
    pub trace: Arc<Mutex<Vec<String>>>,
    /// How long a pull takes, on the test's clock.
    pub pull_millis: std::sync::atomic::AtomicU64,
}
impl FileBridge for BundleBridge {
    fn send<'a>(&'a self, _: &'a Path, _: &'a str) -> VendorFuture<'a, bool> {
        Box::pin(async { Ok(true) })
    }
    /// D54: stopped as the rsync bridge stops a receive: a cancellation returns it, a lost connection makes it `false`.
    fn receive_with<'a>(
        &'a self,
        src: &'a str,
        dst: &'a Path,
        mut stop: marathon_apple::cmd::TransferStop,
    ) -> VendorFuture<'a, bool> {
        use marathon_apple::cmd::TransferStopped;
        Box::pin(async move {
            tokio::select! {
                biased;
                stopped = stop.stopped() => match stopped {
                    TransferStopped::Cancelled(reason) => Err(VendorError::Cancelled(reason)),
                    TransferStopped::Lost(_) => Ok(false),
                },
                pulled = self.receive(src, dst) => pulled,
            }
        })
    }
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool> {
        Box::pin(async move {
            self.pulls.fetch_add(1, Ordering::SeqCst);
            self.trace.lock().unwrap().push(format!("pull:{src}"));
            self.entered.complete(Ok(()));
            let gate = self.gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.wait().await?;
            }
            let millis = self.pull_millis.load(Ordering::SeqCst);
            if millis > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
            }
            match self.mode.load(Ordering::SeqCst) {
                1 => return Ok(false),
                2 => {
                    return Err(VendorError::Other(failure(
                        "java.lang.IllegalStateException",
                        "transport closed",
                    )));
                }
                _ => {}
            }
            let folder = dst.join(src.rsplit('/').next().unwrap());
            tokio::fs::create_dir_all(&folder)
                .await
                .map_err(|e| VendorError::Other(failure("java.io.IOException", e.to_string())))?;
            tokio::fs::write(folder.join("Info.plist"), b"result")
                .await
                .map_err(|e| VendorError::Other(failure("java.io.IOException", e.to_string())))?;
            Ok(true)
        })
    }
}
pub struct Recorder {
    pub provider: Arc<AttachmentProviderDelegate>,
    pub file: jcompat::file_manager::JavaPath,
}
impl Recorder {
    fn attach(&self, test: &Test) -> VendorResult<()> {
        self.provider.on_attachment(
            test,
            Attachment {
                file: self.file.clone(),
                kind: AttachmentType::Log,
                name: None,
            },
        )
    }
}
impl AppleTestRunListener for Recorder {
    fn test_passed<'a>(&'a self, test: &'a Test, _: i64, _: i64) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.attach(test) })
    }
    fn test_failed<'a>(&'a self, test: &'a Test, _: i64, _: i64, _: Option<&'a str>) -> VendorFuture<'a, ()> {
        Box::pin(async move { self.attach(test) })
    }
}
struct Empty;
impl AppleTestRunListener for Empty {}
pub struct Factory {
    pub file: jcompat::file_manager::JavaPath,
    pub fail_finalizer: bool,
    pub cancel_finalizer: bool,
    pub finalizing: Arc<SharedDeferred<()>>,
}
struct Finalizer {
    cancel: bool,
    entered: Arc<SharedDeferred<()>>,
    context: VendorContext,
}
impl AppleTestRunListener for Finalizer {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.entered.complete(Ok(()));
            if self.cancel {
                let reason = self.context.cancellation.cancelled().await;
                Err(VendorError::Cancelled(reason))
            } else {
                Err(VendorError::Other(failure(
                    "java.lang.IllegalStateException",
                    "device log",
                )))
            }
        })
    }
}
impl SimulatorListenerFactory for Factory {
    fn create(
        &self,
        _: Arc<AppleSimulatorDevice>,
        _: DevicePoolId,
        _: BatchRef,
        context: VendorContext,
    ) -> VendorResult<SimulatorListeners> {
        let provider = Arc::new(AttachmentProviderDelegate::default());
        Ok(SimulatorListeners {
            recorder: Arc::new(Recorder {
                provider: provider.clone(),
                file: self.file.clone(),
            }),
            recorder_attachments: vec![provider],
            device_log: if self.fail_finalizer {
                Arc::new(Finalizer {
                    cancel: self.cancel_finalizer,
                    entered: self.finalizing.clone(),
                    context,
                })
            } else {
                Arc::new(Empty)
            },
            data_container_clear: Arc::new(Empty),
        })
    }
}
pub async fn device(
    h: &Harness,
    bridge: Arc<BundleBridge>,
) -> TestResult<(Arc<AppleSimulatorDevice>, Arc<BatchScript>)> {
    let d = h
        .device_on_bridge(
            h.worker.clone(),
            "a",
            Arc::new(WorkerProvisioningContext::new(Transport::Local, h.clock.clone())),
            bridge,
        )
        .await?;
    h.prepare(&d)
        .await
        .map_err(|e| e.failure().to_java_string().to_owned())?;
    let s = Arc::new(BatchScript::default());
    s.attach(&h.worker);
    s.output(passed(1));
    Ok((d, s))
}
pub fn record_cleanup(h: &Harness, trace: Arc<Mutex<Vec<String>>>) {
    let boot = h.boot.clone();
    *h.worker.before.lock().unwrap() = Arc::new(move |c| {
        if c.first().is_some_and(|s| s == "rm") && c.last().is_some_and(|s| s.ends_with(".xcresult")) {
            trace.lock().unwrap().push(c.join(" "));
        }
        let answer = boot.answer(&c);
        Box::pin(async move { Ok(answer) })
    });
}
pub fn result_folders(root: &Path) -> Vec<std::path::PathBuf> {
    let mut result = vec![];
    for entry in std::fs::read_dir(root).unwrap().map(Result::unwrap) {
        let path = entry.path();
        if path.is_dir() {
            if path.extension().is_some_and(|s| s == "xcresult") {
                result.push(path);
            } else {
                result.extend(result_folders(&path));
            }
        }
    }
    result
}
