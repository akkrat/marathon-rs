//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/NmTestParser.kt
use crate::{
    apple_device::AppleDevice,
    apple_device_registry::AppleDeviceRegistry,
    apple_test_bundle_identifier::AppleTestBundleIdentifier,
    bin::kt,
    cmd::{failure, remote::ssh::sshj::config::SshLogging},
    distributed_derived_data::require_distributed_test_bundle,
    model::AppleTestBundle,
};
use jcompat::{
    logging::{Level, Logger},
    throwable::is_error_class,
    writers::JavaString,
};
use marathon_config::{VendorConfiguration, vendor::TestClassRegex};
use marathon_core::{
    error::{VendorError, VendorResult},
    model::Test,
    runtime::{VendorFuture, VendorRuntime},
    vendor::{DeviceRef, RemoteTestParser, VendorContext},
};
use std::sync::Arc;

pub struct NmTestParser {
    pub configuration: Arc<VendorConfiguration>,
    pub registry: Arc<AppleDeviceRegistry>,
    pub identifier: Arc<AppleTestBundleIdentifier>,
    pub test_class_regexes: Vec<TestClassRegex>,
    pub runtime: Arc<dyn VendorRuntime>,
    pub logging: SshLogging,
    logger: Logger,
}
impl NmTestParser {
    pub fn new(
        configuration: Arc<VendorConfiguration>,
        registry: Arc<AppleDeviceRegistry>,
        identifier: Arc<AppleTestBundleIdentifier>,
        test_class_regexes: Vec<TestClassRegex>,
        runtime: Arc<dyn VendorRuntime>,
        logging: SshLogging,
    ) -> Self {
        let logger = logging.logger("NmTestParser");
        Self {
            configuration,
            registry,
            identifier,
            test_class_regexes,
            runtime,
            logging,
            logger,
        }
    }
    pub async fn parse_tests(
        &self,
        device: &dyn AppleDevice,
        bundle: Arc<AppleTestBundle>,
        context: &VendorContext,
    ) -> VendorResult<Vec<Test>> {
        let binary = bundle.test_binary().await?;
        log(
            &self.logging,
            &self.logger,
            Level::Debug,
            format!(
                "Found test binary {} for xctest {}",
                binary.display(),
                bundle.xctest_bundle.display()
            ),
            None,
        );
        let fm = device.remote_file_manager();
        let remote = if self.configuration.skip_copying_derived_data() {
            require_distributed_test_bundle(device, &bundle, &*self.runtime, parsing_error, context).await?
        } else {
            fm.create_remote_directory(None, context).await?;
            fm.create_remote_shared_directory(context).await?;
            let remote = fm.remote_xctest_file();
            if !device.push_shared(&bundle.xctest_bundle, &remote, context).await? {
                return Err(parsing_error("failed to push xctest for test parsing".into()));
            }
            remote
        };
        let mut parts = bundle.relative_binary_path();
        parts.push(binary.file_name().and_then(|n| n.to_str()).unwrap_or_default());
        let remote_binary = fm.join_path(&remote, &parts);
        let nm = &device.binary_environment().nm;
        // join!, rather than try_join!: both remote sessions must have exited before rescan.
        let (swift, objc) = tokio::join!(
            nm.swift_tests(&remote_binary, context),
            nm.objective_c_tests(&remote_binary, context)
        );
        let swift = match swift {
            Ok(lines) => lines,
            Err(error) => {
                self.rescan(&error, &remote_binary)?;
                nm.swift_tests(&remote_binary, context).await?
            }
        };
        let objc = match objc {
            Ok(lines) => lines,
            Err(error) => {
                self.rescan(&error, &remote_binary)?;
                nm.objective_c_tests(&remote_binary, context).await?
            }
        };
        let tests = parse_symbols(&swift, &objc);
        for test in &tests {
            self.identifier.put(test.clone(), bundle.clone());
        }
        Ok(tests
            .into_iter()
            .filter(|test| {
                self.test_class_regexes
                    .iter()
                    .all(|regex| regex.matches(&test.clazz.to_string_lossy()))
            })
            .collect())
    }
    fn rescan(&self, error: &VendorError, binary: &str) -> VendorResult<()> {
        if error.failure().class != JavaString::from("com.malinskiy.marathon.apple.cmd.CommandException") {
            return Err(error.clone());
        }
        log(
            &self.logging,
            &self.logger,
            Level::Debug,
            format!("Scanning {binary} again"),
            Some(error),
        );
        Ok(())
    }
}
impl RemoteTestParser for NmTestParser {
    fn extract(&self, device: DeviceRef, context: VendorContext) -> VendorFuture<'_, Vec<Test>> {
        Box::pin(async move {
            let mut last = None;
            for _ in 0..3 {
                let attempt = async {
                    let apple = self.registry.lookup(&device)?;
                    let bundle =
                        build_bundle(&self.configuration, &*apple, self.runtime.clone(), self.logging.clone()).await?;
                    self.parse_tests(&*apple, bundle, &context).await
                }
                .await;
                match attempt {
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
pub fn parse_symbols(swift: &[String], objc: &[String]) -> Vec<Test> {
    let mut tests = vec![];
    for line in swift {
        let parts = kt::trim(line).split('.').collect::<Vec<_>>();
        if let [pkg, clazz, signature] = parts.as_slice()
            && signature.starts_with("test")
            && !signature.contains("DISABLE")
        {
            tests.push(test(pkg, clazz, signature.split('(').next().unwrap_or_default()));
        }
    }
    for line in objc {
        let parts = kt::trim(line).split(' ').collect::<Vec<_>>();
        if let [clazz, method] = parts.as_slice()
            && method.starts_with("test")
        {
            tests.push(test("", clazz, method));
        }
    }
    tests
}
fn test(pkg: &str, clazz: &str, method: &str) -> Test {
    Test {
        pkg: pkg.into(),
        clazz: clazz.into(),
        method: method.into(),
        meta_properties: vec![],
    }
}
pub(crate) fn parsing_error(message: JavaString) -> VendorError {
    let mut detail = failure("com.malinskiy.marathon.exceptions.TestParsingException", "");
    detail.message = Some(message);
    VendorError::TestParsing(detail)
}
pub(crate) fn log(logging: &SshLogging, logger: &Logger, level: Level, message: String, error: Option<&VendorError>) {
    if logging.enabled(logger, level) {
        (logging.sink)(logger, level, message, error.map(|e| e.failure().clone()));
    }
}
pub(crate) async fn build_bundle(
    config: &VendorConfiguration,
    device: &dyn AppleDevice,
    runtime: Arc<dyn VendorRuntime>,
    logging: SshLogging,
) -> VendorResult<Arc<AppleTestBundle>> {
    let config = config
        .bundle_configuration()
        .filter(|c| c.xctest.is_some())
        .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalArgumentException", "No test bundle provided")))?;
    let path = |p: &jcompat::file_manager::JavaPath| {
        p.to_file_path_buf()
            .map_err(|e| VendorError::Other(failure("java.nio.file.InvalidPathException", e.to_string())))
    };
    let xctest = config
        .xctest
        .as_ref()
        .ok_or_else(|| VendorError::Other(failure("java.lang.IllegalArgumentException", "No test bundle provided")))?;
    Ok(Arc::new(
        AppleTestBundle::new(
            config.app.as_ref().map(path).transpose()?,
            config.test_app.as_ref().map(path).transpose()?,
            path(xctest)?,
            device.sdk(),
            runtime,
            logging,
        )
        .await?,
    ))
}
