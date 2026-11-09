//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/SharedTransfers.kt

use super::{
    TransferStop, TransferStopped, blocking, io_error, remote::ssh::sshj::config::SshLogging, stall_record::StallRecord,
};
use jcompat::{
    file_manager::{absolute_file_path, file_path},
    logging::{Level, Logger},
};
use marathon_core::{
    error::{LeavePoolReason, VendorError, VendorResult},
    runtime::VendorRuntime,
};
use std::path::PathBuf;
use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{Arc, Mutex},
};

type Destination = (String, String);
#[derive(Default)]
struct DestinationState {
    previous: tokio::sync::Mutex<Option<PathBuf>>,
    stalls: StallRecord,
}
type TransferState = Arc<DestinationState>;
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
        mut waiting: TransferStop,
        exists: impl FnOnce() -> E,
        transfer: impl FnOnce() -> T,
    ) -> VendorResult<bool>
    where
        E: Future<Output = VendorResult<Option<bool>>>,
        T: Future<Output = VendorResult<bool>>,
    {
        let path = src.to_string_lossy().into_owned();
        let display_path = file_path(&path).to_string_lossy().into_owned();
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
            .or_default()
            .clone();
        let arrived = state.stalls.ticket();
        // D54: a caller stopped while another's transfer to this destination runs leaves now, and leaves that one running;
        // it records nothing, so who waits behind it goes on.
        let mut previous = tokio::select! {
            biased;
            previous = state.previous.lock() => previous,
            stopped = waiting.stopped() => {
                if let TransferStopped::Lost(lost) = &stopped
                    && self.logging.enabled(&self.logger, Level::Error)
                {
                    let message = format!(
                        "Not sending {display_path} to {worker}:{dst}: the device's connection was lost while this one waited"
                    );
                    (self.logging.sink)(&self.logger, Level::Error, message, Some(lost.clone()));
                }
                return Err(stopped.into_error());
            }
        };
        // U163: whoever waited here while the send to this destination stalled does not start another.
        if let Some(error) = state.stalls.since(arrived) {
            self.logging.log(
                &self.logger,
                Level::Warn,
                || {
                    format!(
                        "Not sending {display_path} to {worker}:{dst}: the send there stalled while this one waited"
                    )
                },
                None,
            );
            return Err(error);
        }
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
        let success = match transfer().await {
            Err(
                error @ VendorError::LeavePool {
                    reason: LeavePoolReason::TransferStalled,
                    ..
                },
            ) => {
                state.stalls.record(error.clone());
                return Err(error);
            }
            other => other?,
        };
        if success {
            *previous = Some(source);
        }
        Ok(success)
    }
}
