//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//! and vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt (executeWorkerCommand).

use crate::cmd::{CommandExecutor, CommandRequest, CommandResult};
use marathon_config::vendor::AppleTimeoutConfiguration;
use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
use std::sync::Arc;

pub trait WorkerCommands: Send + Sync {
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>>;
}

/// Shared by concrete devices and their manager; holds no device reference.
pub struct ExecutorWorkerCommands {
    pub executor: Arc<dyn CommandExecutor>,
    pub timeouts: Arc<AppleTimeoutConfiguration>,
}
impl WorkerCommands for ExecutorWorkerCommands {
    fn execute_worker_command<'a>(
        &'a self,
        command: Vec<String>,
        context: &'a VendorContext,
    ) -> VendorFuture<'a, Option<CommandResult>> {
        execute_worker_command(self.executor.as_ref(), &self.timeouts, command, context)
    }
}

pub fn execute_worker_command<'a>(
    executor: &'a dyn CommandExecutor,
    timeouts: &AppleTimeoutConfiguration,
    command: Vec<String>,
    context: &VendorContext,
) -> VendorFuture<'a, Option<CommandResult>> {
    let mut request = CommandRequest::new(command, timeouts.shell);
    request.idle_timeout = timeouts.shell_idle;
    request.charset = executor.host().charset.clone();
    executor.safe_execute(request, context.clone())
}
