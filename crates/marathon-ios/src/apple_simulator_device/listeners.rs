//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt

use super::*;
use marathon_apple::{
    listener::{
        AppleTestRunListener, CompositeTestRunListener, DebugTestRunListener, DiagnosticLogsPathFinder,
        ResultBundleRunListener, SessionResultsPathFinder, TestResultsListener, TestResultsListenerConfig,
        TestRunListenerAdapter,
    },
    logparser::parser::{DebugLogPrinter, DeviceFailureReason},
};
use marathon_core::{
    artifacts::{FileLogWriter, LogListener},
    listener::{AttachmentProvider, LogTarget},
};

/// 5.3 constructs its recorder first, registering its attachment providers before the log's.
pub struct SimulatorListeners {
    pub recorder: Arc<dyn AppleTestRunListener>,
    pub recorder_attachments: Vec<Arc<dyn AttachmentProvider>>,
    pub device_log: Arc<dyn AppleTestRunListener>,
    pub data_container_clear: Arc<dyn AppleTestRunListener>,
}
pub trait SimulatorListenerFactory: Send + Sync {
    fn create(
        &self,
        device: Arc<AppleSimulatorDevice>,
        pool: DevicePoolId,
        batch: BatchRef,
        context: VendorContext,
    ) -> VendorResult<SimulatorListeners>;
}
struct Empty;
impl AppleTestRunListener for Empty {}
/// The approved dependency seam; 5.3 replaces this default with the production listeners.
#[derive(Default)]
pub struct MinimalSimulatorListenerFactory;
impl SimulatorListenerFactory for MinimalSimulatorListenerFactory {
    fn create(
        &self,
        _: Arc<AppleSimulatorDevice>,
        _: DevicePoolId,
        _: BatchRef,
        _: VendorContext,
    ) -> VendorResult<SimulatorListeners> {
        Ok(SimulatorListeners {
            recorder: Arc::new(Empty),
            recorder_attachments: vec![],
            device_log: Arc::new(Empty),
            data_container_clear: Arc::new(Empty),
        })
    }
}
struct RetireOnFailure(Weak<AppleSimulatorDevice>);
impl AppleTestRunListener for RetireOnFailure {
    fn test_run_failed<'a>(&'a self, _: &'a str, reason: DeviceFailureReason) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if not_fixed_by_setup(reason)
                && let Some(device) = self.0.upgrade()
            {
                device.retire();
            }
            Ok(())
        })
    }
}
pub(super) fn not_fixed_by_setup(reason: DeviceFailureReason) -> bool {
    matches!(
        reason,
        DeviceFailureReason::SystemAppUnavailable | DeviceFailureReason::IncompatibleDevice
    )
}
pub(super) type ExecutionListeners = (CompositeTestRunListener, Vec<Arc<dyn LineListener>>);
impl AppleSimulatorDevice {
    pub(super) fn create_execution_listeners(
        &self,
        pool: DevicePoolId,
        batch: BatchRef,
        deferred: Arc<Deferred<TestBatchResults>>,
        context: VendorContext,
    ) -> VendorResult<ExecutionListeners> {
        let me = self.this()?;
        let extra = self
            .services
            .listeners
            .create(me.clone(), pool.clone(), batch.clone(), context.clone())?;
        let mut attachments = extra.recorder_attachments;
        let log = LogListener::new(
            self.services.runtime.clone(),
            Arc::new(WeakProducer(self.me.clone())),
            Arc::new(FileLogWriter::new(
                self.services.runtime.clone(),
                self.services.output.clone(),
            )),
            LogTarget {
                pool: pool.clone(),
                batch_id: batch.id.clone(),
                device: self.info(),
                test: None,
            },
            Some("xcodebuild-log".into()),
        );
        attachments.push(log.clone());
        let diagnostics = Arc::new(DiagnosticLogsPathFinder::new(self.services.logging.clone()));
        let session = Arc::new(SessionResultsPathFinder::new(self.services.logging.clone()));
        let printer = Arc::new(DebugLogPrinter::new(
            None,
            self.services.vendor.hide_runner_output,
            self.services.logging.clone(),
        ));
        let results = TestResultsListener::new(TestResultsListenerConfig {
            batch: batch.clone(),
            device: DeviceRef(me.clone()),
            deferred,
            timer: self.services.timer.clone(),
            wall: self.services.result_wall.clone(),
            logs: self.services.logs.clone(),
            remote_xcresult: self.remote.remote_xcresult_file(&batch).to_owned(),
            xcresulttool: Arc::new(self.bin.xcrun.xcresulttool.clone()),
            xcode_version: self.xcode_version(),
            context: context.clone(),
            logging: self.services.logging.clone(),
            attachment_providers: attachments,
        })?;
        let listeners: Vec<Arc<dyn AppleTestRunListener>> = vec![
            Arc::new(RetireOnFailure(self.me.clone())),
            Arc::new(results),
            Arc::new(TestRunListenerAdapter { listener: log }),
            extra.device_log,
            Arc::new(DebugTestRunListener::new(
                DeviceRef(me.clone()),
                self.services.logging.clone(),
            )),
            diagnostics.clone(),
            session.clone(),
            extra.recorder,
            extra.data_container_clear,
            Arc::new(ResultBundleRunListener::new(
                me,
                self.services.vendor.xcresult.clone(),
                pool,
                batch,
                self.services.files.clone(),
                Some(self.services.artifacts.clone()),
                context,
                self.services.logging.clone(),
            )),
        ];
        Ok((
            CompositeTestRunListener { listeners },
            vec![diagnostics, session, printer],
        ))
    }
}

struct WeakProducer(Weak<AppleSimulatorDevice>);
impl LogProducer for WeakProducer {
    fn add_line_listener(&self, listener: Arc<dyn LineListener>) -> VendorResult<()> {
        if let Some(device) = self.0.upgrade() {
            device.add_line_listener(listener)?;
        }
        Ok(())
    }
    fn remove_line_listener(&self, listener: &Arc<dyn LineListener>) -> VendorResult<()> {
        if let Some(device) = self.0.upgrade() {
            device.remove_line_listener(listener)?;
        }
        Ok(())
    }
}
