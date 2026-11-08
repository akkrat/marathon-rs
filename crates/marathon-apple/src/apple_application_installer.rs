//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/AppleApplicationInstaller.kt

use crate::{
    apple_device::AppleDevice,
    cmd::{failure, remote::ssh::sshj::config::SshLogging},
    distributed_derived_data::{remote_test_bundle, require_distributed_test_bundle},
    model::{AppleTestBundle, Sdk},
    xctestrun::test_root_factory::{FactoryError, FactoryResult, TestRootFactory},
};
use jcompat::{
    file_manager::JavaPath,
    logging::{Level, Logger},
};
use marathon_config::{CONFIGURATION_EXCEPTION_CLASS, VendorConfiguration, vendor::TestType};
use marathon_core::{
    error::{Cancellation, OperationError, VendorError},
    executor::retry::with_retry_send,
    runtime::VendorRuntime,
    vendor::VendorContext,
};
use std::{borrow::Cow, future::Future, path::PathBuf, pin::Pin, sync::Arc};

/// The hook owner captures its concrete device; no core device downcast is needed.
pub trait AfterInstall: Send + Sync {
    /// Remote parsers share one installer across devices; the default preserves captured-device hooks.
    fn after_install_on<'a>(
        &'a self,
        _device: &'a dyn AppleDevice,
        bundle: &'a AppleTestBundle,
        context: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        self.after_install(bundle, context)
    }

    fn after_install<'a>(
        &'a self,
        bundle: &'a AppleTestBundle,
        context: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>>;
}
pub struct NoopAfterInstall;
impl AfterInstall for NoopAfterInstall {
    fn after_install<'a>(
        &'a self,
        _: &'a AppleTestBundle,
        _: &'a VendorContext,
    ) -> Pin<Box<dyn Future<Output = FactoryResult<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}
pub struct AppleApplicationInstaller<'a> {
    configuration: Cow<'a, VendorConfiguration>,
    runtime: Arc<dyn VendorRuntime>,
    logging: SshLogging,
    logger: Logger,
    hook: &'a dyn AfterInstall,
    owned_hook: Option<Arc<dyn AfterInstall>>,
    code_coverage: bool,
}
fn illegal(message: &str) -> FactoryError {
    VendorError::Other(failure("java.lang.IllegalArgumentException", message)).into()
}
fn path(path: &JavaPath) -> Result<PathBuf, VendorError> {
    path.to_file_path_buf()
        .map_err(|e| VendorError::Other(failure("java.nio.file.InvalidPathException", e.to_string())))
}
fn setup(message: String) -> VendorError {
    let mut detail = failure("com.malinskiy.marathon.exceptions.DeviceSetupException", "");
    detail.message = Some(message);
    VendorError::DeviceSetup(detail)
}
impl<'a> AppleApplicationInstaller<'a> {
    pub fn new(configuration: &'a VendorConfiguration, runtime: Arc<dyn VendorRuntime>, logging: SshLogging) -> Self {
        let logger = logging.logger("com.malinskiy.marathon.apple.AppleApplicationInstaller");
        Self {
            configuration: Cow::Borrowed(configuration),
            runtime,
            logging,
            logger,
            hook: &NoopAfterInstall,
            owned_hook: None,
            code_coverage: false,
        }
    }
    pub fn with_after_install(mut self, hook: &'a dyn AfterInstall) -> Self {
        self.hook = hook;
        self.owned_hook = None;
        self
    }
    /// Vendor services retain parsers as Arc<dyn RemoteTestParser>; own their installer configuration.
    pub fn new_owned(
        configuration: VendorConfiguration,
        runtime: Arc<dyn VendorRuntime>,
        logging: SshLogging,
    ) -> AppleApplicationInstaller<'static> {
        let logger = logging.logger("com.malinskiy.marathon.apple.AppleApplicationInstaller");
        AppleApplicationInstaller {
            configuration: Cow::Owned(configuration),
            runtime,
            logging,
            logger,
            hook: &NoopAfterInstall,
            owned_hook: None,
            code_coverage: false,
        }
    }
    pub fn with_owned_after_install(mut self, hook: Arc<dyn AfterInstall>) -> Self {
        self.owned_hook = Some(hook);
        self
    }
    pub fn with_code_coverage(mut self, enabled: bool) -> Self {
        self.code_coverage = enabled;
        self
    }
    fn log(&self, level: Level, message: String) {
        self.logging.log(&self.logger, level, || message.to_owned(), None);
    }
    fn device_log(&self, prefix: &str, device: &dyn AppleDevice) {
        let mut message = String::from(prefix);
        message.push_str(&device.info().serial_number);
        self.log(Level::Debug, message);
    }
    /// Run on the vendor runtime through the core bridge and await completion.
    pub async fn prepare_installation(
        &self,
        device: &dyn AppleDevice,
        use_xctest_parser: bool,
        context: &VendorContext,
    ) -> FactoryResult<()> {
        let config = self.configuration.bundle_configuration();
        let env = self
            .configuration
            .xctestrun_env()
            .ok_or_else(|| illegal("No xctestrunEnv provided"))?;
        let xcresult = self
            .configuration
            .xcresult_configuration()
            .ok_or_else(|| illegal("No xcresult configuration provided"))?;
        let xctest = config
            .and_then(|c| c.xctest.as_ref())
            .ok_or_else(|| illegal("No test bundle provided"))?;
        let config = config.ok_or_else(|| illegal("No test bundle provided"))?;
        let bundle = AppleTestBundle::new(
            config.app.as_ref().map(path).transpose()?,
            config.test_app.as_ref().map(path).transpose()?,
            path(xctest)?,
            device.sdk(),
            self.runtime.clone(),
            self.logging.clone(),
        )
        .await?;
        let binary = bundle.test_binary().await?;
        let fm = device.remote_file_manager();
        let skip = self.configuration.skip_copying_derived_data();
        let remote = if skip {
            self.device_log("Skip moving xctest to ", device);
            require_distributed_test_bundle(device, &bundle, &*self.runtime, setup, context).await?
        } else {
            self.device_log(
                if bundle.test_application.is_some() {
                    "Moving xctest runner application to "
                } else {
                    "Moving xctest to "
                },
                device,
            );
            let remote = remote_test_bundle(device, &bundle, &*self.runtime).await?;
            let destination = if bundle.test_application.is_some() {
                fm.remote_test_runner_application()
            } else {
                fm.remote_xctest_file()
            };
            let src = bundle.test_application.as_ref().unwrap_or(&bundle.xctest_bundle);
            let mut message = String::from("Error transferring ");
            message.push_str(&String::from_utf16_lossy(&xctest.0.clone()));
            message.push_str(" to ");
            message.push_str(&device.info().serial_number);
            // Keep the last vendor variant intact across the core retry helper's Failure seam.
            let last = std::sync::Mutex::new(None);
            let result = with_retry_send(3, 1000, &*context.clock, context.cancellation.clone(), || async {
                let result = async {
                    fm.create_remote_directory(None, context).await?;
                    fm.create_remote_shared_directory(context).await?;
                    if !device.push_shared(src, &destination, context).await? {
                        return Err(setup(message.clone()));
                    }
                    Ok(())
                }
                .await;
                result.map_err(|error| match error {
                    VendorError::Cancelled(reason) => OperationError::Cancelled(reason),
                    // U163: never retried, here as in the device's own retry. Cancelled is what stops this helper.
                    leave @ VendorError::LeavePool { .. } => {
                        let reason = Cancellation::Requested(leave.failure().clone());
                        *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(leave);
                        OperationError::Cancelled(reason)
                    }
                    other => {
                        let detail = other.failure().clone();
                        *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(other);
                        OperationError::Failed(detail)
                    }
                })
            })
            .await;
            match result {
                Ok(()) => (),
                Err(OperationError::Cancelled(reason)) => {
                    return Err(last
                        .into_inner()
                        .unwrap_or_else(|e| e.into_inner())
                        .filter(|e| matches!(e, VendorError::LeavePool { .. }))
                        .unwrap_or(VendorError::Cancelled(reason))
                        .into());
                }
                Err(OperationError::Failed(detail)) => {
                    return Err(last
                        .into_inner()
                        .unwrap_or_else(|e| e.into_inner())
                        .filter(|e| e.failure() == &detail)
                        .unwrap_or(VendorError::Other(detail))
                        .into());
                }
            }
            remote
        };
        self.device_log("Generating test root for ", device);
        let name = binary.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let mut parts = bundle.relative_binary_path();
        parts.push(name);
        let test_type = self.test_type(device, &fm.join_path(&remote, &parts), context).await?;
        let coverage = config
            .xctestrun
            .as_ref()
            .filter(|_| self.code_coverage)
            .map(path)
            .transpose()?;
        static WARNED_ABOUT_COVERAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if self.code_coverage
            && coverage.is_none()
            && !WARNED_ABOUT_COVERAGE.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.log(Level::Warn, "isCodeCoverageEnabled without bundle.xctestrun: xcodebuild gathers no coverage for the generated xctestrun".into());
        }
        TestRootFactory::new(device, env, xcresult, skip, self.runtime.clone())
            .with_code_coverage(coverage)
            .generate(test_type, &bundle, use_xctest_parser, context)
            .await?;
        self.owned_hook
            .as_deref()
            .unwrap_or(self.hook)
            .after_install_on(device, &bundle, context)
            .await?;
        for extra in config.extra_applications.iter().flatten() {
            let local = path(extra)?;
            let shown = String::from_utf16_lossy(&extra.0.clone());
            let name = shown.to_owned();
            let name = name.rsplit('/').next().unwrap_or("");
            if tokio::fs::metadata(&local).await.is_ok_and(|m| m.is_dir())
                && name.rsplit_once('.').is_some_and(|(_, ext)| ext == "app")
            {
                let mut message = String::from("Installing extra application ");
                message.push_str(&shown);
                message.push_str(" to ");
                message.push_str(&device.info().serial_number);
                self.log(Level::Debug, message);
                let remote = fm.remote_extra_application(name);
                device.push_shared(&local, &remote, context).await?;
                device.install(&remote, context).await?;
            } else {
                let mut message = String::from("Extra application ");
                message.push_str(&shown);
                message.push_str(" should be a directory with extension app");
                self.log(Level::Warn, message);
            }
        }
        Ok(())
    }
    async fn test_type(
        &self,
        device: &dyn AppleDevice,
        binary: &str,
        context: &VendorContext,
    ) -> FactoryResult<TestType> {
        let config = self.configuration.bundle_configuration();
        let app = config.and_then(|c| c.application.as_ref());
        let detected = match config.and_then(|c| c.test_type) {
            Some(value) => value,
            None => {
                let lines = device
                    .binary_environment()
                    .nm
                    .list(binary, &["KIFTestCase".into(), "XCUIApplication".into()], context)
                    .await?;
                if lines.iter().any(|l| l.contains("XCUIApplication"))
                    && !lines.iter().any(|l| l.contains("KIFTestCase"))
                {
                    TestType::Xcuitest
                } else {
                    TestType::Xctest
                }
            }
        };
        let configuration =
            |text: &str| FactoryError::from(VendorError::Other(failure(CONFIGURATION_EXCEPTION_CLASS, text)));
        match (detected, device.sdk(), app.is_some()) {
            (TestType::LogicTest, Sdk::Iphoneos, true) => {
                self.log(
                    Level::Warn,
                    "Overriding test type to XCTest. Reason: target sdk (iPhone) doesn't support running logic tests"
                        .into(),
                );
                Ok(TestType::Xctest)
            }
            (TestType::LogicTest, Sdk::Iphoneos, false) => {
                Err(configuration("Logic tests should target only iPhone Simulator"))
            }
            (TestType::Xctest, Sdk::IphoneSimulator, false) => {
                self.log(Level::Warn, "Overriding test type to Logic Test. Reason: found xctest bundle without application and targeting iPhone Simulator".into());
                Ok(TestType::LogicTest)
            }
            (kind, _, false) if kind != TestType::LogicTest => Err(configuration(&format!(
                "Application is required for test type {}",
                if kind == TestType::Xcuitest {
                    "XCUITEST"
                } else {
                    "XCTEST"
                }
            ))),
            _ => Ok(detected),
        }
    }
}
