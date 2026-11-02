//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/SharedTransfers.kt

use super::{blocking, io_error, remote::ssh::sshj::config::SshLogging};
use jcompat::{
    file_manager::{JavaPath, absolute_file_path, file_path},
    logging::{Level, Logger},
};
use marathon_core::{error::VendorResult, runtime::VendorRuntime};
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
};

type Destination = (String, String);
type TransferState = Arc<tokio::sync::Mutex<Option<JavaPath>>>;
pub struct SharedTransfers {
    destinations: Mutex<HashMap<Destination, TransferState>>,
    runtime: Arc<dyn VendorRuntime>,
    logging: SshLogging,
    logger: Logger,
}
impl SharedTransfers {
    pub fn new(runtime: Arc<dyn VendorRuntime>, logging: SshLogging) -> Self {
        let logger = logging.logger("com.malinskiy.marathon.apple.cmd.SharedTransfers");
        Self {
            destinations: Mutex::new(HashMap::new()),
            runtime,
            logging,
            logger,
        }
    }
    pub async fn send<E, T>(
        &self,
        worker: &str,
        src: &Path,
        dst: &str,
        exists: impl FnOnce() -> E,
        transfer: impl FnOnce() -> T,
    ) -> VendorResult<bool>
    where
        E: Future<Output = VendorResult<Option<bool>>>,
        T: Future<Output = VendorResult<bool>>,
    {
        let path = src.to_string_lossy().into_owned();
        let display_path = String::from_utf16_lossy(&file_path(&path).0);
        let source = blocking(self.runtime.as_ref(), move || {
            let cwd = std::env::current_dir().map_err(io_error)?;
            Ok(absolute_file_path(&path, &cwd.to_string_lossy()))
        })
        .await?;
        let state = self
            .destinations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((worker.to_owned(), dst.to_owned()))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
            .clone();
        let mut previous = state.lock().await;
        let reusable = if previous.as_ref() == Some(&source) {
            match exists().await? {
                Some(value) => value,
                None => {
                    self.logging.log(
                        &self.logger,
                        Level::Warn,
                        || {
                            format!(
                                "Unable to check {worker}:{dst}, reusing the previous transfer of {}",
                                display_path
                            )
                        },
                        None,
                    );
                    true
                }
            }
        } else {
            false
        };
        if reusable {
            self.logging.log(
                &self.logger,
                Level::Debug,
                || format!("{} is already transferred to {worker}:{dst}", display_path),
                None,
            );
            return Ok(true);
        }
        *previous = None;
        let success = transfer().await?;
        if success {
            *previous = Some(source);
        }
        Ok(success)
    }
}
