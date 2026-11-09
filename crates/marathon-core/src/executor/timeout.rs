//! Port of core/src/main/kotlin/com/malinskiy/marathon/extension/Timeout.kt
//!
//! `Duration` overloads of the timeout scopes: `timeout.toMillis()`, then 3.1's machinery.
//! `toMillis` throws ArithmeticException on overflow before any scope starts.
use crate::{error::*, runtime::*};
use jcompat::jtime::Duration;
use marathon_error::FailureKind;
use std::sync::Arc;

fn millis(timeout: &Duration) -> Result<i64, Failure> {
    timeout
        .to_millis()
        .ok_or_else(|| super::shared::failure(FailureKind::Arithmetic, "long overflow"))
}

pub async fn with_timeout<'a>(
    scope: &'a dyn TimeoutScope,
    parent: TaskContext,
    timeout: Duration,
    body: TaskBody<'a>,
) -> OperationResult<()> {
    scope.with_timeout(parent, millis(&timeout)?, body).await
}

pub async fn with_timeout_or_null<'a>(
    scope: &'a dyn TimeoutScope,
    parent: TaskContext,
    timeout: Duration,
    body: TaskBody<'a>,
) -> OperationResult<Option<()>> {
    scope.with_timeout_or_null(parent, millis(&timeout)?, body).await
}

/// The vendor-side twins over `VendorClock`.
pub async fn with_timeout_send<'a>(
    clock: &'a dyn VendorClock,
    parent: Arc<dyn CancellationSignal>,
    timeout: Duration,
    body: VendorTimeoutBody<'a>,
) -> VendorResult<()> {
    clock
        .with_timeout(parent, millis(&timeout).map_err(VendorError::Other)?, body)
        .await
}

pub async fn with_timeout_or_null_send<'a>(
    clock: &'a dyn VendorClock,
    parent: Arc<dyn CancellationSignal>,
    timeout: Duration,
    body: VendorTimeoutBody<'a>,
) -> VendorResult<Option<()>> {
    clock
        .with_timeout_or_null(parent, millis(&timeout).map_err(VendorError::Other)?, body)
        .await
}
