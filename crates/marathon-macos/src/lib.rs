//! macOS device provider, device lifecycle and vendor construction.

use marathon_error::FailureKind;
pub mod device;
pub mod installer;
pub mod provider;
pub mod vendor;

pub use device::{MacosDevice, MacosDeviceOptions};
pub use provider::AppleMacosProvider;
pub use vendor::construct;

fn io_error(error: std::io::Error) -> marathon_core::error::VendorError {
    marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(FailureKind::Io, error.to_string()))
}
fn clock_error(error: marathon_core::time::ReplayError) -> marathon_core::error::VendorError {
    marathon_core::error::VendorError::Other(marathon_apple::cmd::failure(FailureKind::Replay, &error.message))
}
