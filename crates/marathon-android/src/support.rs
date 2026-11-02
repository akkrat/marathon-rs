//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/exception/CommandRejectedException.kt
//! and adam/AdamAndroidDevice.kt (timeout and exception boundaries).

use jcompat::{jtime::Duration, writers::JavaString};
use marathon_adb::client::{AdbError, JavaThrowable};
use marathon_core::{error::*, runtime::*, vendor::VendorContext};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
pub(crate) fn failure(class: &str, message: impl Into<JavaString>) -> Failure {
    Failure {
        class: class.into(),
        message: Some(message.into()),
        cause: None,
        stacktrace: None,
    }
}
pub(crate) fn other(class: &str, message: impl Into<JavaString>) -> VendorError {
    VendorError::Other(failure(class, message))
}
pub(crate) fn setup(message: impl Into<JavaString>) -> VendorError {
    VendorError::DeviceSetup(failure(
        "com.malinskiy.marathon.exceptions.DeviceSetupException",
        message,
    ))
}
pub(crate) fn transfer(message: impl Into<JavaString>) -> VendorError {
    VendorError::Transfer(failure("com.malinskiy.marathon.exceptions.TransferException", message))
}
pub(crate) fn install(message: impl Into<JavaString>) -> VendorError {
    other("com.malinskiy.marathon.android.exception.InstallException", message)
}
pub(crate) fn io(e: std::io::Error) -> VendorError {
    other("java.io.IOException", e.to_string().as_str())
}
pub(crate) fn throwable(t: JavaThrowable) -> Failure {
    Failure {
        class: t.class.into(),
        message: t.message.map(|s| s.as_str().into()),
        cause: t.cause.map(|c| Box::new(throwable(*c))),
        stacktrace: None,
    }
}
pub fn adb_error(e: AdbError) -> VendorError {
    VendorError::Other(throwable(e.to_throwable()))
}
pub fn execution_error(e: VendorError) -> VendorError {
    let class = &e.failure().class;
    if *class == "com.malinskiy.adam.exception.RequestRejectedException".into()
        || *class == "com.malinskiy.marathon.android.exception.CommandRejectedException".into()
    {
        let mut f = failure(
            "com.malinskiy.marathon.exceptions.DeviceLostException",
            e.failure().to_java_string(),
        );
        f.cause = Some(Box::new(e.failure().clone()));
        VendorError::DeviceLost(f)
    } else {
        e
    }
}
pub(crate) fn millis(d: Duration) -> VendorResult<i64> {
    d.to_millis()
        .ok_or_else(|| other("java.lang.ArithmeticException", "long overflow"))
}
pub(crate) async fn cancellable<T>(
    signal: Arc<dyn CancellationSignal>,
    future: impl Future<Output = VendorResult<T>>,
) -> VendorResult<T> {
    tokio::select! { biased; reason = signal.cancelled() => Err(VendorError::Cancelled(reason)), result = future => result }
}
/// Timeout bodies observe their child signal and finish cleanup before the clock returns.
pub(crate) async fn timed<'a, T: Send + 'a>(
    context: &'a VendorContext,
    ms: i64,
    body: impl FnOnce(Arc<dyn CancellationSignal>) -> VendorFuture<'a, T> + Send + 'a,
) -> VendorResult<Option<T>> {
    let result = Mutex::new(None);
    let slot = &result;
    let completed = context
        .clock
        .with_timeout_or_null(
            context.cancellation.clone(),
            ms,
            Box::new(move |signal| {
                Box::pin(async move {
                    *lock(slot) = Some(body(signal).await?);
                    Ok(())
                })
            }),
        )
        .await?;
    if completed.is_none() {
        return Ok(None);
    }
    Ok(lock(&result).take())
}
pub(crate) async fn blocking<T: Send + 'static>(
    runtime: &dyn VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime
        .spawn_blocking(Box::new(move || {
            let _ = tx.send(body());
            Ok(())
        }))
        .join()
        .await?;
    rx.await.map_err(|_| {
        other(
            "java.lang.IllegalStateException",
            "blocking task finished without a result",
        )
    })?
}
pub(crate) fn dex_error(e: marathon_dex::DexError) -> VendorError {
    fn convert(e: marathon_dex::DexError) -> Failure {
        Failure {
            class: e.jvm_class,
            message: e.message,
            cause: e.cause.map(|c| Box::new(convert(*c))),
            stacktrace: None,
        }
    }
    VendorError::Other(convert(e))
}
pub(crate) fn trim(s: &str) -> String {
    JavaString(jcompat::jstring::trim(&JavaString::from(s).0).to_vec()).to_string_lossy()
}
