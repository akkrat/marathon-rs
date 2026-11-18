//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/Retry.kt
use crate::{error::*, runtime::JobRef, time::SchedulerClock};
use marathon_error::FailureKind;
use std::future::Future;
pub async fn with_retry<T, F, Fut>(
    attempts: i32,
    delay_millis: i64,
    clock: &dyn SchedulerClock,
    job: JobRef,
    body: F,
) -> OperationResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = OperationResult<T>>,
{
    with_retry_until(attempts, delay_millis, None, clock, job, body, |_| {}).await
}

/// D87: [`with_retry`] that makes no attempt at or after `deadline`, a time on `clock`. Whether a retry may start is
/// checked where it would start, after its delay, and also before the delay, so that no delay is spent on a retry that
/// could not start. A retry that may not start is not made: the failure of the attempt before it is returned after
/// `cut` is told how many attempts were made. An attempt already running is never stopped by the deadline. `None` is
/// [`with_retry`].
pub async fn with_retry_until<T, F, Fut>(
    attempts: i32,
    delay_millis: i64,
    deadline: Option<i64>,
    clock: &dyn SchedulerClock,
    job: JobRef,
    mut body: F,
    cut: impl FnOnce(i32),
) -> OperationResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = OperationResult<T>>,
{
    if attempts <= 0 {
        return Err(super::shared::failure(FailureKind::IllegalArgument, "attempts must be positive").into());
    }
    let may_start = |after: i64| deadline.is_none_or(|deadline| clock.now_millis().saturating_add(after) < deadline);
    for attempt in 1..=attempts {
        match body().await {
            Ok(value) => return Ok(value),
            Err(OperationError::Cancelled(reason)) => return Err(reason.into()),
            Err(error) if attempt == attempts => return Err(error),
            Err(error) => {
                if !may_start(delay_millis) {
                    cut(attempt);
                    return Err(error);
                }
                clock.delay(delay_millis, job.clone()).await?;
                if !may_start(0) {
                    cut(attempt);
                    return Err(error);
                }
            }
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
    body: F,
) -> OperationResult<T>
where
    T: Send,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = OperationResult<T>> + Send,
{
    with_retry_send_gated(attempts, delay_millis, clock, cancellation, &|| true, body).await
}

/// D87: [`with_retry_send`] whose retries start only while `may_start` says so, asked before a retry's delay and again
/// where it would start, after the delay. A retry that may not start is not made, and the failure before it is returned.
pub async fn with_retry_send_gated<T, F, Fut>(
    attempts: i32,
    delay_millis: i64,
    clock: &dyn crate::runtime::VendorClock,
    cancellation: std::sync::Arc<dyn crate::runtime::CancellationSignal>,
    may_start: &(dyn Fn() -> bool + Send + Sync),
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
            Err(error) if !may_start() => return Err(error),
            Err(error) => {
                clock
                    .delay(delay_millis, cancellation.clone())
                    .await
                    .map_err(|error| match error {
                        VendorError::Cancelled(reason) => OperationError::Cancelled(reason),
                        other => OperationError::Failed(other.failure().clone()),
                    })?;
                if !may_start() {
                    return Err(error);
                }
            }
        }
    }
    unreachable!("positive attempts return on their last iteration")
}
