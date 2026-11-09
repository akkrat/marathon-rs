//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Retry.kt
use crate::{error::*, runtime::JobRef, time::SchedulerClock};
use marathon_error::FailureKind;
use std::future::Future;
pub async fn with_retry<T, F, Fut>(
    attempts: i32,
    delay_millis: i64,
    clock: &dyn SchedulerClock,
    job: JobRef,
    mut body: F,
) -> OperationResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = OperationResult<T>>,
{
    if attempts <= 0 {
        return Err(super::shared::failure(FailureKind::IllegalArgument, "attempts must be positive").into());
    }
    for attempt in 1..=attempts {
        match body().await {
            Ok(value) => return Ok(value),
            Err(OperationError::Cancelled(reason)) => return Err(reason.into()),
            Err(error) if attempt == attempts => return Err(error),
            Err(_) => clock.delay(delay_millis, job.clone()).await?,
        }
    }
    unreachable!("positive attempts return on their last iteration")
}

/// Send twin for vendor-side services, using the run's scheduler timeline.
pub async fn with_retry_send<T, F, Fut>(
    attempts: i32,
    delay_millis: i64,
    clock: &dyn crate::runtime::VendorClock,
    cancellation: std::sync::Arc<dyn crate::runtime::CancellationSignal>,
    mut body: F,
) -> OperationResult<T>
where
    T: Send,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = OperationResult<T>> + Send,
{
    if attempts <= 0 {
        return Err(super::shared::failure(FailureKind::IllegalArgument, "attempts must be positive").into());
    }
    for attempt in 1..=attempts {
        if let Some(reason) = cancellation.reason() {
            return Err(reason.into());
        }
        let outcome = match futures_util::future::select(Box::pin(body()), cancellation.cancelled()).await {
            futures_util::future::Either::Left((result, _)) => result,
            futures_util::future::Either::Right((reason, _)) => return Err(reason.into()),
        };
        match outcome {
            Ok(value) => return Ok(value),
            Err(OperationError::Cancelled(reason)) => return Err(reason.into()),
            Err(error) if attempt == attempts => return Err(error),
            Err(_) => clock
                .delay(delay_millis, cancellation.clone())
                .await
                .map_err(|error| match error {
                    VendorError::Cancelled(reason) => OperationError::Cancelled(reason),
                    other => OperationError::Failed(other.failure().clone()),
                })?,
        }
    }
    unreachable!("positive attempts return on their last iteration")
}
