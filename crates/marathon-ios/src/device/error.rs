//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/SimulatorFactory.kt
//! (original exception and suppressed disposal errors), and SimulatorProvisioner.kt (SetupDropped).

use marathon_apple::{cmd::failure, device::connection_factory::ConnectionError};
use marathon_core::{
    error::{Failure, VendorError},
    time::ReplayError,
};
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupDropped(pub String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupError {
    Connection(Box<ConnectionError>),
    Dropped(SetupDropped),
}
/// Retained until logging/output: core Failure deliberately has no suppressed field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisioningError {
    pub original: SetupError,
    pub suppressed: Vec<ProvisioningError>,
}
pub type ProvisioningResult<T> = Result<T, ProvisioningError>;
pub type ProvisioningFuture<'a, T> = Pin<Box<dyn Future<Output = ProvisioningResult<T>> + Send + 'a>>;
impl ProvisioningError {
    pub fn is_cancelled(&self) -> bool {
        matches!(&self.original, SetupError::Connection(error) if matches!(error.as_ref(), ConnectionError::Vendor(VendorError::Cancelled(_) | VendorError::LeavePool { .. })))
    }
    pub fn cancellation(&self) -> Option<marathon_core::error::Cancellation> {
        match &self.original {
            SetupError::Connection(error) => match error.as_ref() {
                ConnectionError::Vendor(VendorError::Cancelled(reason)) => Some(reason.clone()),
                ConnectionError::Vendor(VendorError::LeavePool { detail, .. }) => {
                    Some(marathon_core::error::Cancellation::Requested(detail.clone()))
                }
                _ => None,
            },
            _ => None,
        }
    }
    pub fn failure(&self) -> Failure {
        match &self.original {
            SetupError::Connection(error) => error.as_ref().clone().into_vendor().failure().clone(),
            SetupError::Dropped(SetupDropped(message)) => Failure {
                class: "com.malinskiy.marathon.apple.ios.device.SetupDropped".into(),
                message: Some(message.to_owned()),
                cause: None,
                stacktrace: None,
            },
        }
    }
    /// D12 omits JVM frames, while preserving the Throwable suppression relationship.
    pub fn stack_trace(&self) -> String {
        self.output_failure().stack_trace()
    }
    /// Bridge/5.2 boundary: retain the variant and suppression text without reopening core Failure.
    pub fn into_vendor(self) -> VendorError {
        let detail = self.output_failure();
        let original = match self.original {
            SetupError::Connection(error) => (*error).into_vendor(),
            SetupError::Dropped(_) => return VendorError::Other(detail),
        };
        use marathon_core::error::Cancellation;
        match original {
            VendorError::Cancelled(reason) => VendorError::Cancelled(match reason {
                Cancellation::Requested(_) => Cancellation::Requested(detail),
                Cancellation::Parent(_) => Cancellation::Parent(detail),
                Cancellation::Timeout { scope, .. } => Cancellation::Timeout { scope, detail },
            }),
            VendorError::LeavePool { reason, .. } => VendorError::LeavePool { reason, detail },
            VendorError::DeviceLost(_) => VendorError::DeviceLost(detail),
            VendorError::DeviceSetup(_) => VendorError::DeviceSetup(detail),
            VendorError::IncompatibleDevice(_) => VendorError::IncompatibleDevice(detail),
            VendorError::BatchExecution(_) => VendorError::BatchExecution(detail),
            VendorError::TestParsing(_) => VendorError::TestParsing(detail),
            VendorError::NoDevices(_) => VendorError::NoDevices(detail),
            VendorError::Transfer(_) => VendorError::Transfer(detail),
            VendorError::Other(_) => VendorError::Other(detail),
        }
    }
    pub fn output_failure(&self) -> Failure {
        let mut result = self.failure();
        if !self.suppressed.is_empty() {
            // Failure's stacktrace is its frame/body portion, not another exception header.
            let mut body = result.stacktrace.clone().unwrap_or_default();
            if !body.is_empty() {
                body.push('\n');
            }
            for suppressed in &self.suppressed {
                body.push_str("\tSuppressed: ");
                for (i, line) in suppressed.stack_trace().to_owned().split_inclusive('\n').enumerate() {
                    if i != 0 {
                        body.push('\t');
                    }
                    body.push_str(line);
                }
            }
            result.stacktrace = Some(body);
        }
        result
    }
}
impl From<ConnectionError> for ProvisioningError {
    fn from(error: ConnectionError) -> Self {
        Self {
            original: SetupError::Connection(Box::new(error)),
            suppressed: Vec::new(),
        }
    }
}
impl From<VendorError> for ProvisioningError {
    fn from(error: VendorError) -> Self {
        ConnectionError::Vendor(error).into()
    }
}
impl From<SetupDropped> for ProvisioningError {
    fn from(error: SetupDropped) -> Self {
        Self {
            original: SetupError::Dropped(error),
            suppressed: Vec::new(),
        }
    }
}
pub fn clock_error(error: ReplayError) -> VendorError {
    VendorError::Other(failure("ReplayError", &error.message))
}
impl std::fmt::Display for ProvisioningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.failure().to_java_string().to_owned())
    }
}
impl std::error::Error for ProvisioningError {}
