//! Test-end artifact work that runs beside the batch's later tests (D80); Rust-only support for the port of
//! vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt.
use marathon_apple::listener::AppleTestRunListener;
use marathon_core::{
    error::{VendorError, VendorResult},
    runtime::{CancellationSignal, VendorFuture, VendorRuntime, VendorTask},
};
use std::sync::{Arc, Mutex};

/// One batch's test-end artifact work, run one item at a time in the order the tests ended.
///
/// Kotlin stops the recorder, transcodes and pulls a test's video inside the test-ended callback, and the runner
/// handles the xcodebuild events one at a time, so a slow stop or pull held back the next test's start and end
/// (U201). Items run off that path instead. They stay serialized, so a worker transcodes and sends one video of a
/// batch at a time, as before, and `settle` is the point before which the batch's results are not built.
#[derive(Default)]
pub struct PendingArtifacts {
    last: Mutex<Option<Arc<dyn VendorTask>>>,
    error: Arc<Mutex<Option<VendorError>>>,
}
impl PendingArtifacts {
    /// `cancellation` is the batch's: once it is cancelled, items not yet started are skipped, and their
    /// cancellation is not reported, since the batch reports its own. Returns the item's task, which ends once the
    /// item and every item before it have.
    pub fn submit(
        &self,
        runtime: &dyn VendorRuntime,
        cancellation: Arc<dyn CancellationSignal>,
        work: VendorFuture<'static, ()>,
    ) -> Arc<dyn VendorTask> {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let previous = last.take();
        let error = self.error.clone();
        let task = runtime.spawn(Box::pin(async move {
            if let Some(previous) = previous
                && let Err(e) = previous.join().await
            {
                // An item records its own failures and then succeeds, so a failed join is one it could not record:
                // a panic, which the runtime reports as the task's failure.
                keep(&error, e);
            }
            let result = match cancellation.reason() {
                Some(reason) => Err(VendorError::Cancelled(reason)),
                None => work.await,
            };
            if let Err(e) = result
                && !(matches!(e, VendorError::Cancelled(_)) && cancellation.reason().is_some())
            {
                keep(&error, e);
            }
            Ok(())
        }));
        *last = Some(task.clone());
        task
    }
    /// Waits for every item submitted so far. A failure is kept for `take_error`, so that the waiting listener's
    /// neighbours still run.
    pub async fn settle(&self) {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(last) = last
            && let Err(e) = last.join().await
        {
            keep(&self.error, e);
        }
    }
    /// The first failure of an item, which the listener reports as Kotlin's inline transfer would have.
    pub fn take_error(&self) -> VendorResult<()> {
        match self.error.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// The ledger keeps the first failure, in the order the items ran.
fn keep(ledger: &Mutex<Option<VendorError>>, error: VendorError) {
    ledger.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(error);
}

/// Settles the batch's pending artifacts at the start of `after_test_run`, before the results listener reports the
/// batch, so every attachment a test earned is in its result.
pub struct ArtifactBarrier(pub Arc<PendingArtifacts>);
impl AppleTestRunListener for ArtifactBarrier {
    fn after_test_run(&self) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            self.0.settle().await;
            Ok(())
        })
    }
}
