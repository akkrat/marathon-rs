//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/BaseAndroidDevice.kt:236-301
use crate::{device::AndroidDevice, listeners::*, models::select_recorder_type, support::*};
use marathon_config::Configuration;
use marathon_core::{
    artifacts::{FileLogWriter, LogListener},
    error::*,
    execution::result::TestRunResultsAccumulator,
    listener::{AttachmentProvider, LogTarget},
    model::*,
    runtime::*,
    vendor::Device,
};
use marathon_error::FailureKind;
use std::sync::{Arc, Mutex};

pub struct InstalledListener {
    pub listener: Arc<dyn AndroidTestRunListener>,
    pub attachments: Option<Arc<dyn AttachmentProvider>>,
}
/// WP 6.6b supplies per-batch listeners, retaining provider identity through completion.
pub trait AndroidListenerFactory: Send + Sync {
    fn create(
        &self,
        device: &AndroidDevice,
        configuration: &Configuration,
        pool: &DevicePoolId,
        batch: &BatchRef,
    ) -> VendorResult<InstalledListener>;
}
pub struct ListenerSlots {
    pub recorder: Option<Arc<dyn AndroidListenerFactory>>,
    pub screen_capture: Option<Arc<dyn AndroidListenerFactory>>,
    pub file_sync: Option<Arc<dyn AndroidListenerFactory>>,
    pub profiling: Option<Arc<dyn AndroidListenerFactory>>,
}
impl Default for ListenerSlots {
    fn default() -> Self {
        Self {
            recorder: Some(Arc::new(ArtifactFactory::Recorder)),
            screen_capture: Some(Arc::new(ArtifactFactory::Adam)),
            file_sync: Some(Arc::new(ArtifactFactory::FileSync)),
            profiling: Some(Arc::new(ArtifactFactory::Profiling)),
        }
    }
}
enum ArtifactFactory {
    Recorder,
    Adam,
    FileSync,
    Profiling,
}
impl AndroidListenerFactory for ArtifactFactory {
    fn create(
        &self,
        device: &AndroidDevice,
        configuration: &Configuration,
        pool: &DevicePoolId,
        batch: &BatchRef,
    ) -> VendorResult<InstalledListener> {
        use crate::artifact_listeners::*;

        let artifacts = ArtifactContext {
            device: device.clone(),
            pool: pool.clone(),
            batch: batch.clone(),
        };
        fn provider<T: AndroidTestRunListener + AttachmentProvider + 'static>(listener: T) -> InstalledListener {
            let listener = Arc::new(listener);
            InstalledListener {
                listener: listener.clone(),
                attachments: Some(listener),
            }
        }
        let no_op = || InstalledListener {
            listener: Arc::new(NoOpTestRunListener),
            attachments: None,
        };
        let c = &device.runtime().configuration;
        Ok(match self {
            Self::Recorder => {
                match select_recorder_type(&device.info().device_features, &c.screen_record_configuration) {
                    Some(DeviceFeature::Video) => provider(video::ScreenRecorderTestBatchListener::new(
                        artifacts,
                        configuration.screen_recording_policy,
                    )),
                    Some(DeviceFeature::Screenshot) => provider(screenshot::ScreenCapturerTestRunListener::new(
                        artifacts,
                        configuration.screen_recording_policy,
                    )),
                    None => no_op(),
                }
            }
            Self::Adam => provider(adam_capture::AdamScreenCaptureTestRunListener::new(artifacts)),
            Self::FileSync => InstalledListener {
                listener: Arc::new(file_sync::FileSyncTestRunListener::new(artifacts)),
                attachments: None,
            },
            Self::Profiling if c.profiling_configuration.enabled && c.profiling_configuration.pbtxt.is_some() => {
                provider(profiling::ProfilingRunListener::new(artifacts))
            }
            Self::Profiling => no_op(),
        })
    }
}
fn missing(name: &str) -> VendorError {
    other(
        FailureKind::UnsupportedOperation,
        format!("Android {name} listener is not installed").as_str(),
    )
}
pub fn create(
    device: &AndroidDevice,
    configuration: &Configuration,
    pool: DevicePoolId,
    batch: BatchRef,
    deferred: Arc<dyn Deferred<TestBatchResults>>,
    slots: &ListenerSlots,
) -> VendorResult<CompositeTestRunListener> {
    let runtime = device.runtime();
    let deps = &runtime.dependencies;
    let mut providers: Vec<Arc<dyn AttachmentProvider>> = vec![];
    let mut slot = |factory: &Option<Arc<dyn AndroidListenerFactory>>,
                    enabled: bool,
                    name: &str|
     -> VendorResult<Arc<dyn AndroidTestRunListener>> {
        if let Some(factory) = factory {
            let installed = factory.create(device, configuration, &pool, &batch)?;
            providers.extend(installed.attachments);
            Ok(installed.listener)
        } else if enabled {
            Err(missing(name))
        } else {
            Ok(Arc::new(NoOpTestRunListener))
        }
    };
    let c = &runtime.configuration;
    let recorder = slot(
        &slots.recorder,
        select_recorder_type(&device.info().device_features, &c.screen_record_configuration).is_some(),
        "recorder",
    )?;
    let profiling = slot(
        &slots.profiling,
        c.profiling_configuration.enabled && c.profiling_configuration.pbtxt.is_some(),
        "profiling",
    )?;
    let log = LogListener::new(
        deps.io.clone(),
        Arc::new(device.clone()),
        Arc::new(FileLogWriter::new(deps.io.clone(), deps.files.clone())),
        LogTarget {
            pool: pool.clone(),
            batch_id: batch.id.clone(),
            device: device.info(),
            test: None,
        },
        Some("logcat".into()),
    );
    // End the closure borrow before adding the log provider between profiling and screen.
    drop(slot);
    providers.push(log.clone());
    let mut slot = |factory: &Option<Arc<dyn AndroidListenerFactory>>,
                    enabled: bool,
                    name: &str|
     -> VendorResult<Arc<dyn AndroidTestRunListener>> {
        if let Some(factory) = factory {
            let installed = factory.create(device, configuration, &pool, &batch)?;
            providers.extend(installed.attachments);
            Ok(installed.listener)
        } else if enabled {
            Err(missing(name))
        } else {
            Ok(Arc::new(NoOpTestRunListener))
        }
    };
    let file_sync = slot(
        &slots.file_sync,
        !runtime.file_sync.pull.is_empty() || !runtime.file_sync.push.is_empty(),
        "file sync",
    )?;
    let screen = if slots.screen_capture.is_some() {
        slot(&slots.screen_capture, false, "screen capture")?
    } else {
        ArtifactFactory::Adam
            .create(device, configuration, &pool, &batch)?
            .listener
    };
    let attachments = TestResultsListener::collector(&providers)?;
    let log_runtime = runtime.clone();
    let accumulator = TestRunResultsAccumulator::new(deps.timer.clone(), Arc::new(marathon_core::logging::host_millis))
        .map_err(|e| other(FailureKind::ReplayClock, e.message))?;
    let results = TestResultsListener {
        accumulator: AccumulatingResultTestRunListener {
            result: Mutex::new(accumulator),
            logs: Arc::new(move |record| {
                let logger = log_runtime.component(&record.logger);
                (log_runtime.dependencies.log)(&logger, record.level, &record.message, record.cause.as_ref());
            }),
        },
        batch,
        device: device.reference(),
        deferred,
        attachments,
        log: {
            let device = device.clone();
            Arc::new(move |level, text| {
                device.log_named(
                    "marathon_android::listener_factory",
                    level,
                    &text.to_owned(),
                    None,
                )
            })
        },
    };
    Ok(CompositeTestRunListener {
        listeners: vec![
            recorder,
            Arc::new(TestRunListenerAdapter { listener: log }),
            Arc::new(results),
            Arc::new(DebugTestRunListener { device: device.clone() }),
            screen,
            file_sync,
            profiling,
        ],
    })
}
