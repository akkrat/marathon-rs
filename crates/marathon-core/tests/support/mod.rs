//! Port of core/src/main/kotlin/com/malinskiy/marathon/Marathon.kt (test driver seam).
#![allow(dead_code)]
use marathon_core::{
    error::*,
    executor::{CoreExecutor, test_support::ControlledReplay},
    runtime::*,
};
use marathon_error::FailureKind;
use std::rc::Rc;
pub fn run(
    body: impl FnOnce(Rc<CoreExecutor>, Rc<ControlledReplay>) -> LocalFuture<'static, OperationResult<()>> + Send + 'static,
) {
    let result = CoreExecutor::run_virtual(body);
    assert_eq!(result, Ok(()));
}
pub fn cancelled(message: &str) -> Cancellation {
    marathon_core::executor::shared::requested(message)
}
pub fn failed(message: &str) -> Failure {
    marathon_core::executor::shared::failure(FailureKind::IllegalState, message)
}
pub fn replay<T>(result: marathon_core::time::ClockResult<T>) -> T {
    result.unwrap_or_else(|e| panic!("replay failure: {e:?}"))
}
pub fn completed(job: &JobRef) -> Completion {
    match job.state() {
        JobState::Completed(result) => result,
        other => panic!("expected completion, got {other:?}"),
    }
}
