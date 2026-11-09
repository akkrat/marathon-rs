//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/device/AppleSimulatorProvider.kt:155–274

use super::*;
use marathon_config::{
    binding::ConfigurationError,
    dynamic::{FrameDecoder, FrameError, MAX_FRAME_PAYLOAD},
    loader::{DevicesProvider, devices_load_error_to_configuration_error},
    pipeline::{LoadError, bind_devices_bytes},
    vendor::DeviceProvider as Source,
};
use marathon_error::FailureKind;
use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};

async fn blocking<T: Send + Sync + Clone + 'static>(
    runtime: &dyn VendorRuntime,
    work: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let done = SharedDeferred::new();
    let result = done.clone();
    let task = runtime.spawn_blocking(Box::new(move || {
        result.complete(work());
        Ok(())
    }));
    let result = done.wait().await.map(|v| (*v).clone());
    task.join().await?;
    result
}
fn config_failure(error: ConfigurationError) -> marathon_core::error::Failure {
    let mut cause = None;
    for e in error.causes.into_iter().rev() {
        cause = Some(Box::new(marathon_core::error::Failure {
            kind: e.kind,
            message: e.message,
            cause,
            stacktrace: None,
        }));
    }
    marathon_core::error::Failure {
        kind: error.kind,
        message: error.message,
        cause,
        stacktrace: None,
    }
}
fn mapping(error: &ConfigurationError) -> bool {
    matches!(error.kind, FailureKind::Config(kind) if kind.is_mapping())
}
fn load_error(error: LoadError, wrapper: &str) -> VendorError {
    match error {
        LoadError::Configuration(error) if mapping(&error) => {
            let mut detail = failure(FailureKind::NoDevices, wrapper);
            detail.cause = Some(Box::new(config_failure(*error)));
            VendorError::NoDevices(detail)
        }
        other => VendorError::Other(config_failure(*devices_load_error_to_configuration_error(
            other,
            DevicesProvider::Ios,
        ))),
    }
}
fn file_read_error(file: &Path, error: &io::Error) -> VendorError {
    let reason = match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::IsADirectory => "Is a directory".to_owned(),
        _ => error.to_string(),
    };
    other(FailureKind::FileNotFound, format!("{} ({reason})", file.display()))
}
pub(super) async fn load(provider: &AppleSimulatorProvider) -> VendorResult<Option<MarathonDevices>> {
    let binding = provider.services.binding.clone();
    let this = provider.this()?;
    let vendor = provider.services.vendor.clone();
    blocking(provider.services.runtime.as_ref(), move || {
        let file = vendor
            .devices_file
            .as_ref()
            .map(|p| PathBuf::from(p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| {
                PathBuf::from(binding.process_dir.to_string_lossy().into_owned()).join("Marathondevices")
            });
        let file = if file.is_absolute() {
            file
        } else {
            PathBuf::from(binding.process_dir.to_string_lossy().into_owned()).join(file)
        };
        if !matches!(vendor.device_provider, Source::Static { .. }) && !file.exists() {
            return Ok(None);
        }
        this.log(Level::DEBUG, || "Using static device provider".into(), None);
        let bytes = std::fs::read(&file).map_err(|e| file_read_error(&file, &e))?;
        let devices = bind_devices_bytes(&bytes, &binding)
            .map_err(|e| load_error(e, &format!("Invalid Marathondevices file {} format", file.display())))?;
        if devices.workers.is_empty() {
            return Err(VendorError::NoDevices(failure(
                FailureKind::NoDevices,
                format!("No workers found in the {}", file.display()),
            )));
        }
        Ok(Some(devices))
    })
    .await
}
async fn cancelled<T>(caller: &VendorContext, work: impl Future<Output = T>) -> VendorResult<T> {
    tokio::select! { biased; reason = caller.cancellation.cancelled() => Err(VendorError::Cancelled(reason)), result = work => Ok(result) }
}
struct Snapshot {
    devices: MarathonDevices,
    accepted: oneshot::Sender<()>,
}
impl AppleSimulatorProvider {
    pub(super) async fn monitor(
        self: Arc<Self>,
        initial: Option<MarathonDevices>,
        caller: VendorContext,
    ) -> VendorResult<()> {
        if let Some(initial) = initial {
            self.process(initial, caller.clone())
                .await
                .map_err(ProvisioningError::into_vendor)?;
            loop {
                self.services.recovery.reconnect(self.clone(), caller.clone()).await?;
                caller.clock.delay(16, caller.cancellation.clone()).await?;
            }
        }
        let Source::Dynamic { host, port } = &self.services.vendor.device_provider else {
            return Err(other(
                FailureKind::IllegalState,
                "Missing dynamic provider configuration",
            ));
        };
        let (host, port) = (host.to_owned(), *port);
        let (tx, mut rx) = mpsc::channel::<Snapshot>(1);
        let this = self.clone();
        let producer_caller = caller.clone();
        let producer =
            self.services.runtime.spawn(Box::pin(
                async move { this.produce(host, port, tx, producer_caller).await },
            ));
        let result = async {
            loop {
                if let Ok(snapshot) = rx.try_recv() {
                    // Rendezvous completes on receive, before processing the snapshot.
                    let _ = snapshot.accepted.send(());
                    self.process(snapshot.devices, caller.clone())
                        .await
                        .map_err(ProvisioningError::into_vendor)?;
                }
                self.services.recovery.reconnect(self.clone(), caller.clone()).await?;
                caller.clock.delay(16, caller.cancellation.clone()).await?;
            }
        }
        .await;
        self.monitor_cancel.cancel(requested("Monitoring stopped"));
        let _ = producer.join().await;
        result
    }
    async fn produce(
        &self,
        host: String,
        port: i32,
        tx: mpsc::Sender<Snapshot>,
        caller: VendorContext,
    ) -> VendorResult<()> {
        let port = match u16::try_from(port) {
            Ok(port) => port,
            Err(_) => {
                self.source_error(other(FailureKind::IllegalArgument, format!("port out of range:{port}")));
                return Ok(());
            }
        };
        loop {
            let connected = cancelled(&caller, TcpStream::connect((host.as_str(), port))).await?;
            let mut socket = match connected {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    self.log(
                        Level::WARN,
                        || "Connection refused, retrying in 5 seconds...".into(),
                        None,
                    );
                    caller.clock.delay(5000, caller.cancellation.clone()).await?;
                    continue;
                }
                Err(error) => {
                    self.source_error(other(FailureKind::Io, error.to_string()));
                    return Ok(());
                }
            };
            let result = self.read_socket(&mut socket, &tx, &caller).await;
            let _ = socket.shutdown().await;
            if let Err(VendorError::Cancelled(reason)) = result {
                return Err(VendorError::Cancelled(reason));
            }
            if let Err(error) = result {
                self.source_error(error);
            }
            return Ok(()); // A1: the producer ends; monitoring continues polling the closed source.
        }
    }
    fn source_error(&self, error: VendorError) {
        let detail = error.failure().clone();
        self.log(
            Level::ERROR,
            || {
                format!(
                    "Error occurred: {}",
                    detail
                        .message
                        .as_ref()
                        .map(|s| s.to_owned())
                        .unwrap_or_else(|| "null".into())
                )
            },
            Some(detail.clone()),
        );
    }
    async fn read_socket(
        &self,
        socket: &mut TcpStream,
        tx: &mpsc::Sender<Snapshot>,
        caller: &VendorContext,
    ) -> VendorResult<()> {
        loop {
            // Read exactly one frame. Decoding a coalesced successor before the rendezvous send
            // would move its binding failure ahead of the source's send suspension.
            let mut decoder = FrameDecoder::new();
            let mut header = vec![0; 8];
            let mut read = 0;
            while read < header.len() {
                let n = cancelled(caller, socket.read(&mut header[read..]))
                    .await?
                    .map_err(|e| other(FailureKind::Io, e.to_string()))?;
                if n == 0 {
                    let _ = decoder.feed(&header[..read], &self.services.binding);
                    return Err(frame_error(
                        decoder
                            .finish()
                            .and_then(Result::err)
                            .ok_or_else(|| other(FailureKind::IllegalState, "Missing EOF failure"))?,
                    ));
                }
                read += n;
            }
            let length = i32::from_be_bytes([header[4], header[5], header[6], header[7]]);
            if (0..=MAX_FRAME_PAYLOAD).contains(&length) {
                header.resize(8 + length as usize, 0);
                while read < header.len() {
                    let n = cancelled(caller, socket.read(&mut header[read..]))
                        .await?
                        .map_err(|e| other(FailureKind::Io, e.to_string()))?;
                    if n == 0 {
                        let _ = decoder.feed(&header[..read], &self.services.binding);
                        return Err(frame_error(
                            decoder
                                .finish()
                                .and_then(Result::err)
                                .ok_or_else(|| other(FailureKind::IllegalState, "Missing EOF failure"))?,
                        ));
                    }
                    read += n;
                }
            }
            let binding = self.services.binding.clone();
            let devices = blocking(self.services.runtime.as_ref(), move || {
                FrameDecoder::new()
                    .feed(&header, &binding)
                    .pop()
                    .ok_or_else(|| other(FailureKind::IllegalState, "Incomplete frame"))?
                    .map_err(frame_error)
            })
            .await?;
            let (accepted, received) = oneshot::channel();
            cancelled(caller, tx.send(Snapshot { devices, accepted }))
                .await?
                .map_err(|_| other(FailureKind::ChannelSendClosed, "Channel was closed"))?;
            cancelled(caller, received)
                .await?
                .map_err(|_| other(FailureKind::ChannelSendClosed, "Channel was closed"))?;
        }
    }
}
fn frame_error(error: FrameError) -> VendorError {
    match error {
        FrameError::Binding(error) => {
            load_error(LoadError::Configuration(error), "Invalid Marathondevices update format")
        }
        error => other(error.failure_kind().unwrap_or(FailureKind::Runtime), error.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_read_errors_use_java_reason_text() {
        for (kind, reason) in [
            (io::ErrorKind::NotFound, "No such file or directory"),
            (io::ErrorKind::IsADirectory, "Is a directory"),
            (io::ErrorKind::PermissionDenied, "Permission denied"),
        ] {
            let error = file_read_error(Path::new("/Marathondevices"), &io::Error::from(kind));
            assert_eq!(error.failure().kind.description(), "file not found".to_owned());
            assert_eq!(error.failure().message, Some(format!("/Marathondevices ({reason})")));
        }
    }
}
