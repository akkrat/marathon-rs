//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/XCTestParser.kt
//!
//! D20 (PLAN.md §3.11): Kotlin loads libxctest-parser.dylib from its own resources. This program neither contains nor
//! ships that third-party library, so an `xctest` run reads a copy the user supplies. What follows the read, the push to
//! the worker and the fake test run, is the port.
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
    fs::File,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// D20: names the directory that holds the user's copy of the library.
pub const PARSER_DIR_VARIABLE: &str = "MARATHON_XCTEST_PARSER_DIR";
const LIBRARY: &str = "libxctest-parser.dylib";
/// The SDKs Kotlin has a resource for; physical device SDKs have none and keep failing per attempt (S3).
pub const SIMULATOR_SDKS: [Sdk; 4] = [
    Sdk::IphoneSimulator,
    Sdk::TvSimulator,
    Sdk::WatchSimulator,
    Sdk::VisionSimulator,
];
const CONFIGURATION_EXCEPTION: &str = "com.malinskiy.marathon.config.exceptions.ConfigurationException";

/// Where an `xctest` run finds the user's copy of the library. Only the `xctest` branch of a vendor builds this from
/// the environment, so an `nm` run never reads the variable.
#[derive(Clone, Debug, Default)]
pub struct ParserLibraries {
    directory: Option<PathBuf>,
}
impl ParserLibraries {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: Some(directory.into()),
        }
    }
    pub fn from_environment() -> Self {
        Self {
            directory: std::env::var_os(PARSER_DIR_VARIABLE)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }
    }
    /// The Kotlin resource layout: `<platform name>/libxctest-parser.dylib`.
    fn file(&self, sdk: Sdk) -> Option<PathBuf> {
        let directory = self.directory.as_ref().filter(|_| is_supplied(sdk))?;
        Some(directory.join(sdk.platform_name()).join(LIBRARY))
    }
    /// Blocks on the file system. Passes when one of `sdks` has a readable file.
    fn check(&self, sdks: &[Sdk]) -> Result<(), String> {
        let Some(directory) = &self.directory else {
            return Err(format!("the environment variable {PARSER_DIR_VARIABLE} is not set"));
        };
        let mut problems = Vec::new();
        for sdk in sdks {
            match open_regular_file(&directory.join(sdk.platform_name()).join(LIBRARY)) {
                Ok(_) => return Ok(()),
                Err(problem) => problems.push(problem),
            }
        }
        Err(problems.join("; "))
    }
    /// The vendor's check before any connection or simulator exists. An iOS vendor only learns the platform from the
    /// device it gets, so it passes all simulator SDKs and `extract` checks the device's own platform.
    pub async fn check_at_construction(
        &self,
        sdks: &[Sdk],
        runtime: &dyn marathon_core::runtime::VendorRuntime,
    ) -> VendorResult<()> {
        let libraries = self.clone();
        let sdks = sdks.to_vec();
        blocking(runtime, move || {
            libraries.check(&sdks).map_err(|problem| {
                VendorError::Other(crate::cmd::failure(CONFIGURATION_EXCEPTION, supply_message(&problem)))
            })
        })
        .await
    }
}
/// Blocks on the file system. Without O_NONBLOCK, opening a FIFO that has no writer would wait forever.
fn open_regular_file(path: &Path) -> Result<File, String> {
    let unreadable = |error: std::io::Error| format!("{} cannot be read ({error})", path.display());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
        .open(path)
        .map_err(unreadable)?;
    if file.metadata().map_err(unreadable)?.is_file() {
        Ok(file)
    } else {
        Err(format!("{} is not a file", path.display()))
    }
}
fn is_supplied(sdk: Sdk) -> bool {
    sdk == Sdk::Macos || SIMULATOR_SDKS.contains(&sdk)
}
fn supply_message(problem: &str) -> String {
    format!(
        "The xctest test parser needs {LIBRARY}, which this program does not ship: {problem}. Set {PARSER_DIR_VARIABLE} \
         to a directory holding <platform name>/{LIBRARY} (for example MacOSX/{LIBRARY}), with a copy obtained under its \
         EULA from a Kotlin marathon distribution. The library is third-party software under its own EULA, which states \
         that it sends error reports to its server. If your tests are not generated at runtime, use the nm test parser \
         (testParserConfiguration type nm), which needs no library."
    )
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
    libraries: ParserLibraries,
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
        libraries: ParserLibraries,
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
            libraries,
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
        let library = self.libraries.file(sdk).ok_or_else(|| {
            parsing_error(format!("No libxctest-parser.dylib for platform {}", sdk.platform_name()).into())
        })?;
        let temp = blocking(&*self.runtime, move || {
            let mut source =
                open_regular_file(&library).map_err(|problem| parsing_error(supply_message(&problem).into()))?;
            let mut file = tempfile::NamedTempFile::new().map_err(io_error)?;
            std::io::copy(&mut source, &mut file).map_err(io_error)?;
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
            // D20: retrying cannot make a missing library appear, so this fails before the attempts and the install.
            if let Some(sdk) = self
                .registry
                .lookup(&device)
                .ok()
                .map(|apple| apple.sdk())
                .filter(|sdk| is_supplied(*sdk))
            {
                let libraries = self.libraries.clone();
                blocking(&*self.runtime, move || {
                    libraries
                        .check(&[sdk])
                        .map_err(|problem| parsing_error(supply_message(&problem).into()))
                })
                .await?;
            }
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
                    // U163: a stalled send is not worth two more on this device. Core's retry may borrow another.
                    Err(e @ (VendorError::Cancelled(_) | VendorError::LeavePool { .. })) => return Err(e),
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
