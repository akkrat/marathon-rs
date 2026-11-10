//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt (lines 609–676 and 1089–1123)
use marathon_error::FailureKind;
pub mod data_container_clear;
pub mod device_log;
pub mod screenshot;
pub mod video;

use crate::apple_simulator_device::{AppleSimulatorDevice, SimulatorListenerFactory, SimulatorListeners};
use marathon_apple::bin::kt::trim;
use marathon_apple::{apple_device::AppleDevice, listener::AppleTestRunListener};
use marathon_core::{
    artifacts::AttachmentProviderDelegate,
    error::{OperationError, VendorError, VendorResult},
    listener::AttachmentProvider,
    model::*,
    output::*,
    runtime::*,
    vendor::{Device, VendorContext},
};
use marathon_logging::{Component, Level};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ListenerContext {
    pub device: Arc<AppleSimulatorDevice>,
    pub pool: DevicePoolId,
    pub batch: BatchRef,
    pub context: VendorContext,
}
impl ListenerContext {
    fn component(&self, name: &str) -> Component {
        self.device.services().logging.component(name)
    }
    fn log(&self, logger: &Component, level: Level, message: String) {
        let logging = &self.device.services().logging;
        if logging.enabled(logger, level) {
            (logging.sink)(logger, level, message, None);
        }
    }
    fn millis(&self, label: &'static str) -> VendorResult<i64> {
        self.device
            .services()
            .wall
            .millis(label)
            .map_err(|e| VendorError::Other(marathon_apple::cmd::failure(FailureKind::ReplayClock, &e.message)))
    }
    async fn file(&self, kind: FileType, test: Option<&Test>) -> VendorResult<PathBuf> {
        self.device
            .services()
            .output
            .create_file(&FileTarget {
                kind,
                pool: self.pool.clone(),
                device: self.device.info(),
                test: test.cloned(),
                test_batch_id: Some(self.batch.id.clone()),
                id: None,
                chunk: None,
            })
            .await
            .map_err(|e| match e {
                OperationError::Cancelled(c) => VendorError::Cancelled(c),
                OperationError::Failed(f) => VendorError::Other(f),
            })
    }
    fn policy(&self, failed: bool) -> bool {
        self.device.services().configuration.screen_recording_policy
            == marathon_config::schema::ScreenRecordingPolicy::OnAny
            || failed
    }
    async fn cancel(&self, job: &Mutex<Option<Arc<dyn VendorTask>>>) -> VendorResult<()> {
        let job = job.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(job) = job {
            job.cancel(marathon_apple::cmd::cancelled("Recording finished"));
            // Kotlin cancelAndJoin does not rethrow the joined child's failure.
            let _ = job.join().await;
        }
        Ok(())
    }
}

pub(crate) async fn blocking<T: Send + 'static>(
    io: &dyn VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = io.spawn_blocking(Box::new(move || {
        let _ = tx.send(body());
        Ok(())
    }));
    task.join().await?;
    rx.await
        .map_err(|e| VendorError::Other(marathon_apple::cmd::failure(FailureKind::IllegalState, e.to_string())))?
}

#[derive(Default)]
pub struct ProductionSimulatorListenerFactory;
struct Empty;
impl AppleTestRunListener for Empty {}
impl SimulatorListenerFactory for ProductionSimulatorListenerFactory {
    fn create(
        &self,
        device: Arc<AppleSimulatorDevice>,
        pool: DevicePoolId,
        batch: BatchRef,
        context: VendorContext,
    ) -> VendorResult<SimulatorListeners> {
        let c = ListenerContext {
            device,
            pool,
            batch,
            context,
        };
        let selected = crate::recorder_type_selector::select_recorder_type(
            &c.device.info().device_features,
            &c.device.services().vendor.screen_record_configuration,
        );
        let (recorder, recorder_attachments): (Arc<dyn AppleTestRunListener>, Vec<Arc<dyn AttachmentProvider>>) =
            match selected {
                Some(DeviceFeature::Video) => {
                    let l = Arc::new(video::ScreenRecordingListener::new(c.clone()));
                    (l.clone(), vec![l.attachments.clone()])
                }
                Some(DeviceFeature::Screenshot) => {
                    let l = Arc::new(screenshot::ScreenCapturerTestRunListener::new(c.clone()));
                    (l.clone(), vec![l.attachments.clone()])
                }
                None => (Arc::new(Empty), vec![]),
            };
        Ok(SimulatorListeners {
            recorder,
            recorder_attachments,
            device_log: Arc::new(device_log::DeviceLogListener::new(c.clone())),
            data_container_clear: Arc::new(data_container_clear::DataContainerClearListener(c)),
        })
    }
}
