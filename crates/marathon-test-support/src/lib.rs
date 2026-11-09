//! Test support shared across crates: StubVendor/StubDevice, the FakeSimulatorWorker world, fake
//! executors and golden-file helpers.
//!
//! See PLAN.md §4.1.

pub mod apple;
pub mod assert;
pub mod console;
pub mod differential;
pub mod factory;
pub mod scenario;
pub mod stub_bundle;
pub mod stub_device;
pub mod stub_provider;

pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub mod recorded_errors;
