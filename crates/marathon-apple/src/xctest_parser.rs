//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/XCTestParser.kt
use crate::{
    apple_application_installer::AppleApplicationInstaller,
    apple_device::AppleDevice,
    apple_device_registry::AppleDeviceRegistry,
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    cmd::{blocking, io_error, remote::ssh::sshj::config::SshLogging},
    model::{AppleTestBundle, Sdk},
    nm_test_parser::{build_bundle, log, parsing_error},
    test::{TestEvent, TestRequest},
    xcodebuild_session::{CLOSE_TIMEOUT, XcodebuildSession},
};
use jcompat::{
    jtime::Duration,
    logging::{Level, Logger},
    throwable::is_error_class,
    writers::JavaString,
};
use marathon_config::VendorConfiguration;
use marathon_core::{
    error::{VendorError, VendorResult},
    listener::LineListener,
    model::Test,
    runtime::{VendorFuture, VendorRuntime},
    vendor::{DeviceRef, RemoteTestParser, VendorContext},
};
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

pub fn embedded_dylib(sdk: Sdk) -> Option<&'static [u8]> {
    macro_rules! lib {
        ($p:literal) => {
            include_bytes!(concat!(
                "../resources/libxctest-parser/",
                $p,
                "/libxctest-parser.dylib"
            ))
            .as_slice()
        };
    }
    Some(match sdk {
        Sdk::IphoneSimulator => lib!("iPhoneSimulator"),
        Sdk::Macos => lib!("MacOSX"),
        Sdk::TvSimulator => lib!("AppleTVSimulator"),
        Sdk::WatchSimulator => lib!("WatchSimulator"),
        Sdk::VisionSimulator => lib!("XRSimulator"),
        _ => return None,
    })
}
#[derive(Default)]
struct Buffer(Mutex<JavaString>);
impl LineListener for Buffer {
    fn on_line<'a>(&'a self, line: &'a JavaString) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let mut buffer = self.0.lock().unwrap_or_else(|e| e.into_inner());
            buffer.append(line.clone());
            buffer.push_str("\n");
            Ok(())
        })
    }
}
pub struct XCTestParser<'a> {
    pub configuration: Arc<VendorConfiguration>,
    pub registry: Arc<AppleDeviceRegistry>,
    pub identifier: Arc<AppleTestBundleIdentifier>,
    pub installer: AppleApplicationInstaller<'a>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub logging: SshLogging,
    pub session_close_timeout: Duration,
    logger: Logger,
    buffer: Arc<Buffer>,
}
impl<'a> XCTestParser<'a> {
    pub fn new(
        configuration: Arc<VendorConfiguration>,
        registry: Arc<AppleDeviceRegistry>,
        identifier: Arc<AppleTestBundleIdentifier>,
        installer: AppleApplicationInstaller<'a>,
        runtime: Arc<dyn VendorRuntime>,
        logging: SshLogging,
    ) -> Self {
        let logger = logging.logger("XCTestParser");
        Self {
            configuration,
            registry,
            identifier,
            installer,
            runtime,
            logging,
            session_close_timeout: CLOSE_TIMEOUT,
            logger,
            buffer: Arc::default(),
        }
    }
    async fn parse_tests(
        &self,
        device: &dyn AppleDevice,
        bundle: Arc<AppleTestBundle>,
        context: &VendorContext,
    ) -> VendorResult<Vec<Test>> {
        self.installer
            .prepare_installation(device, true, context)
            .await
            .map_err(|e| e.into_vendor())?;
        let sdk = device.sdk();
        let dylib = embedded_dylib(sdk).ok_or_else(|| {
            parsing_error(format!("No libxctest-parser.dylib for platform {}", sdk.platform_name()).into())
        })?;
        let temp = blocking(&*self.runtime, move || {
            let mut file = tempfile::NamedTempFile::new().map_err(io_error)?;
            file.write_all(dylib).map_err(io_error)?;
            Ok(file)
        })
        .await?;
        let destination = device.remote_file_manager().remote_xctest_parser_file();
        let pushed = device.push_file(temp.path(), &destination, context).await;
        // Approved S2: delete even after a failed push, off core.
        blocking(&*self.runtime, move || {
            drop(temp);
            Ok(())
        })
        .await?;
        if !pushed? {
            return Err(parsing_error(
                "failed to push libparse-tests.dylib for test parsing".into(),
            ));
        }
        self.collect_tests(device, bundle, context).await
    }
    /// Setup and resource transfer precede this collection seam, as in the Kotlin tests.
    pub async fn collect_tests(
        &self,
        device: &dyn AppleDevice,
        bundle: Arc<AppleTestBundle>,
        context: &VendorContext,
    ) -> VendorResult<Vec<Test>> {
        let fm = device.remote_file_manager();
        let remote = fm.remote_xctestrun_file();
        log(
            &self.logging,
            &self.logger,
            Level::Debug,
            format!("Remote xctestrun = {remote}"),
            None,
        );
        let request = TestRequest::new(fm.parent_of(&remote).to_owned(), remote, false);
        let session = XcodebuildSession::new(self.runtime.clone(), self.logging.clone());
        let listener: Arc<dyn LineListener> = self.buffer.clone();
        *self.buffer.0.lock().unwrap_or_else(|e| e.into_inner()) = JavaString::default();
        let mut channel = None;
        let mut tests = Vec::new();
        let mut result = async {
            device.add_line_listener(listener.clone())?;
            channel = Some(device.execute_test_request(request, session.clone(), context).await?);
            if let Some(channel) = &channel {
                while let Some(events) = channel.receive(context).await? {
                    for event in events {
                        if let TestEvent::TestStarted { mut id } = event {
                            id.pkg = bundle.test_bundle_id().into();
                            if !tests.contains(&id) {
                                tests.push(id);
                            }
                        }
                    }
                }
            }
            log(
                &self.logging,
                &self.logger,
                Level::Debug,
                "Execution finished".into(),
                None,
            );
            Ok(())
        }
        .await;
        if let Err(error @ VendorError::Cancelled(_)) = &result {
            log(
                &self.logging,
                &self.logger,
                Level::Error,
                "Test parsing got stuck. You can increase the timeout in settings if it's too strict".into(),
                Some(error),
            );
            result = Ok(());
        }
        if let Some(channel) = channel {
            channel.cancel();
        }
        if let Some(error) = session.stop(self.session_close_timeout, context).await {
            log(
                &self.logging,
                &self.logger,
                Level::Warn,
                format!(
                    "xcodebuild session of test parsing on {} has not been closed",
                    device.info().serial_number.to_string_lossy()
                ),
                Some(&VendorError::Other(error)),
            );
        }
        device.remove_line_listener(&listener)?;
        result?;
        if tests.is_empty() {
            log(
                &self.logging,
                &self.logger,
                Level::Warn,
                format!(
                    "XCTestParser failed to parse tests. xcodebuild output:\n{}",
                    self.buffer
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .to_string_lossy()
                ),
                None,
            );
        }
        for test in &tests {
            self.identifier.put(test.clone(), bundle.clone());
        }
        Ok(tests)
    }
}
impl RemoteTestParser for XCTestParser<'_> {
    fn extract(&self, device: DeviceRef, context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async move {
            let mut last = None;
            for _ in 0..3 {
                let result = async {
                    let apple = self.registry.lookup(&device)?;
                    let bundle =
                        build_bundle(&self.configuration, &*apple, self.runtime.clone(), self.logging.clone()).await?;
                    self.parse_tests(&*apple, bundle, &context).await
                }
                .await;
                match result {
                    Ok(tests) => return Ok(tests),
                    Err(e @ VendorError::Cancelled(_)) => return Err(e),
                    Err(error) => {
                        if !is_error_class(&error.failure().class.to_string_lossy()) {
                            log(
                                &self.logging,
                                &self.logger,
                                Level::Debug,
                                "Remote parsing failed. Retrying".into(),
                                Some(&error),
                            );
                        }
                        last = Some(error);
                    }
                }
            }
            Err(last.unwrap_or_else(|| parsing_error("Remote parsing failed".into())))
        })
    }
}
