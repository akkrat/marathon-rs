//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt (executeTestRequest)
//! and vendor/vendor-apple/macos/src/main/kotlin/com/malinskiy/marathon/apple/macos/MacosDevice.kt (executeTestRequest).
use crate::{
    apple_device::AppleDevice,
    bin::kt,
    cmd::{consume_output, remote::ssh::sshj::config::SshLogging},
    logparser::{parser::TestRunProgressParser, producer::XctestEventProducer},
    test::TestRequest,
    xcodebuild_session::{EventReceiver, XcodebuildSession},
};
use jcompat::{logging::Level, writers::JavaString};
use marathon_core::{
    error::{VendorError, VendorResult},
    listener::LineListener,
    time::Timer,
    vendor::VendorContext,
};
use std::sync::{Arc, Mutex};

/// One gate serializes stdout/stderr callbacks, including the suspending listeners.
#[derive(Default)]
pub struct LineListeners {
    entries: Mutex<Vec<Arc<dyn LineListener>>>,
    gate: tokio::sync::Mutex<()>,
}
impl LineListeners {
    pub fn add(&self, listener: Arc<dyn LineListener>) {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).push(listener);
    }
    pub fn remove(&self, listener: &Arc<dyn LineListener>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = entries.iter().position(|e| Arc::ptr_eq(e, listener)) {
            entries.remove(index);
        }
    }
    pub async fn on_line(&self, line: &JavaString) -> VendorResult<()> {
        let _gate = self.gate.lock().await;
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for entry in entries {
            entry.on_line(line).await?;
        }
        Ok(())
    }
}
/// The device checks its previous session, then records the new one, before delegating here.
/// `args` is simulator_test_args on iOS, raw xcodebuildTestArgs on macOS.
pub fn execute_test_request(
    device: Arc<dyn AppleDevice>,
    request: TestRequest,
    session: Arc<XcodebuildSession>,
    context: VendorContext,
    output: TestOutput,
) -> EventReceiver {
    let opener = device.clone();
    let TestOutput {
        label,
        args,
        timer,
        listeners,
        logging,
    } = output;
    let producer = XctestEventProducer::new(TestRunProgressParser::new(
        timer,
        request.test_target_name.clone().unwrap_or_default(),
        logging.clone(),
    ));
    session.produce(
        context,
        move |ctx| async move {
            opener
                .binary_environment()
                .xcrun
                .xcodebuild
                .test_without_building(opener.udid(), opener.sdk(), &request, &args, &ctx)
                .await
        },
        move |command, sender, ctx| async move {
            let producer = tokio::sync::Mutex::new(producer);
            let exit = consume_output(
                &command.base,
                &ctx,
                |line| {
                    let producer = &producer;
                    let sender = &sender;
                    let listeners = &listeners;
                    async move {
                        let events = producer
                            .lock()
                            .await
                            .process(&line)
                            .map_err(|e| VendorError::Other(e.to_failure()))?;
                        // A20: an event is visible before its line reaches the listeners.
                        if let Some(events) = events {
                            sender.send(events).await?;
                        }
                        listeners.on_line(&JavaString::from(line)).await
                    }
                },
                |line| {
                    let logging = &logging;
                    let label = &label;
                    let listeners = &listeners;
                    async move {
                        if kt::trim(&line).is_empty() {
                            return Ok(());
                        }
                        let (logger, message) = {
                            let mut registry = logging.registry.lock().unwrap_or_else(|e| e.into_inner());
                            let logger = registry.xcodebuild();
                            (logger, registry.xcodebuild_message(label, &line))
                        };
                        if let Some(message) = message {
                            (logging.sink)(&logger, Level::Warn, message, None);
                        }
                        listeners.on_line(&JavaString::from(line)).await
                    }
                },
            )
            .await?;
            if logging.enabled(device.logger(), Level::Debug) {
                (logging.sink)(
                    device.logger(),
                    Level::Debug,
                    format!(
                        "Finished test batch execution with exit status {}",
                        exit.map_or_else(|| "null".into(), |v| v.to_string())
                    ),
                    None,
                );
            }
            Ok(())
        },
    )
}
pub struct TestOutput {
    pub label: String,
    pub args: Vec<(String, String)>,
    pub timer: Arc<dyn Timer>,
    pub listeners: Arc<LineListeners>,
    pub logging: SshLogging,
}
