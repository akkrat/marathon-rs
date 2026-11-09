//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/cmd/FakeCommandExecutor.kt

use marathon_logging::{Component, LogSettings};

use super::{CommandExecutor, CommandHost, CommandRequest, CommandResult, CommandSession, local::KotlinProcessHost};
use jcompat::{
    jtime::Duration,
    
};
use marathon_core::{
    runtime::{VendorFuture, VendorRuntime},
    vendor::VendorContext,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub struct FakeCommandExecutor {
    runtime: Arc<dyn VendorRuntime>,
    result: CommandResult,
    host: CommandHost,
    logger: Component,
    pub commands: Mutex<Vec<Vec<String>>>,
    pub envs: Mutex<Vec<BTreeMap<String, String>>>,
    pub timeout: Mutex<Option<Duration>>,
}
impl FakeCommandExecutor {
    pub fn new(runtime: Arc<dyn VendorRuntime>, result: CommandResult, logging: &mut LogSettings) -> Self {
        Self {
            runtime,
            result,
            host: KotlinProcessHost::host(),
            logger: logging.component("marathon_apple::cmd::test_support"),
            commands: Mutex::new(Vec::new()),
            envs: Mutex::new(Vec::new()),
            timeout: Mutex::new(None),
        }
    }
}
impl CommandExecutor for FakeCommandExecutor {
    fn host(&self) -> &CommandHost {
        &self.host
    }
    fn component(&self) -> &Component {
        &self.logger
    }
    fn connected(&self) -> bool {
        true
    }
    fn log_error(&self, _: String) {}
    fn execute(&self, request: CommandRequest, _: VendorContext) -> VendorFuture<'_, CommandSession> {
        Box::pin(async move {
            *self.timeout.lock().unwrap_or_else(|e| e.into_inner()) = Some(request.timeout);
            self.envs.lock().unwrap_or_else(|e| e.into_inner()).push(request.env);
            self.commands
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request.command);
            Ok(CommandSession::finished(self.runtime.clone(), self.result.clone()))
        })
    }
    fn close(&self) -> VendorFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
