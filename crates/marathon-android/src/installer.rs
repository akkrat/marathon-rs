//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/AndroidAppInstaller.kt
use crate::{device::AndroidDevice, models::AndroidTestBundle, services::AndroidRuntime, support::*};
use marathon_core::{error::*, vendor::VendorContext};
use marathon_error::{AndroidKind, FailureKind};
use marathon_logging::Level;
use std::path::PathBuf;
use std::sync::Arc;
pub struct AndroidAppInstaller {
    runtime: Arc<AndroidRuntime>,
}
impl AndroidAppInstaller {
    pub fn new(runtime: Arc<AndroidRuntime>) -> Self {
        Self { runtime }
    }
    fn log(&self, level: Level, message: &str, error: Option<&Failure>) {
        self.runtime.emit(
            &self.runtime.component("marathon_android::installer"),
            level,
            message,
            error,
        )
    }
    pub async fn prepare_installation(&self, device: &AndroidDevice, context: VendorContext) -> VendorResult<()> {
        for bundle in &self.runtime.bundles {
            self.prepare_partial_installation(device, bundle.clone(), context.clone())
                .await?;
        }
        Ok(())
    }
    pub async fn prepare_partial_installation(
        &self,
        device: &AndroidDevice,
        bundle: Arc<AndroidTestBundle>,
        context: VendorContext,
    ) -> VendorResult<()> {
        let b = bundle.clone();
        let info = blocking(self.runtime.dependencies.io.as_ref(), move || b.instrumentation_info()).await?;
        let serial = device.serial_number().to_owned();
        self.log(
            Level::DEBUG,
            &format!("Installing application output to {serial}"),
            None,
        );
        if let Some(app) = &bundle.files.application {
            // N25: the source rejects plain APKs, not split APKs, below API 21.
            if bundle.files.split_apks.is_empty() && device.api_level() < 21 {
                return Err(other(
                    FailureKind::Android(AndroidKind::DeviceNotSupported),
                    "Device api level should be more then 20",
                ));
            }
            self.reinstall(
                device,
                &info.application_package,
                app,
                &bundle.files.split_apks,
                context.clone(),
            )
            .await?;
            self.appops(device, &info.application_package, context.clone()).await?;
        }
        for extra in &bundle.files.extra_applications {
            self.log(Level::DEBUG, &format!("Installing extra application to {serial}"), None);
            let path = extra.clone();
            let package = blocking(self.runtime.dependencies.io.as_ref(), move || {
                marathon_dex::apk_parser::parse_app_package_name(&path).map_err(dex_error)
            })
            .await?;
            self.reinstall(device, &package, extra, &[], context.clone()).await?;
        }
        self.log(
            Level::DEBUG,
            &format!("Installing instrumentation package to {serial}"),
            None,
        );
        self.reinstall(
            device,
            &info.instrumentation_package,
            &bundle.files.test_application,
            &[],
            context,
        )
        .await?;
        self.log(
            Level::DEBUG,
            &format!("Prepare installation finished for {serial}"),
            None,
        );
        Ok(())
    }
    async fn appops(&self, device: &AndroidDevice, package: &str, context: VendorContext) -> VendorResult<()> {
        if !self.runtime.configuration.mock_location {
            return Ok(());
        }
        if device.api_level() < 23 {
            self.log(
                Level::WARN,
                &format!(
                    "Can't setup mock location: device {} doesn't support appops",
                    device.serial_number().to_owned()
                ),
                None,
            );
            return Ok(());
        }
        let package = package.to_owned();
        let r = device
            .critical_execute_shell_command(
                &format!("appops set {package} android:mock_location allow"),
                "",
                context.clone(),
            )
            .await?;
        if r.exit_code != 0 {
            let r = device
                .critical_execute_shell_command("appops query-op android:mock_location allow", "", context)
                .await?;
            self.log(Level::ERROR,&format!("Can't set android:mock_location on {package}. List of apps currently using android:mock_location:{}",r.output.to_owned()),None)
        }
        Ok(())
    }
    async fn reinstall(
        &self,
        device: &AndroidDevice,
        package: &str,
        app: &std::path::Path,
        splits: &[PathBuf],
        context: VendorContext,
    ) -> VendorResult<()> {
        let paths: Vec<_> = std::iter::once(app)
            .chain(splits.iter().map(PathBuf::as_path))
            .map(|p| {
                // absolute() does not validate NUL; these paths precede worker commands.
                if p.as_os_str().as_encoded_bytes().contains(&0) {
                    return Err(io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Nul character not allowed",
                    )));
                }
                std::path::absolute(p)
                    .map_err(io)
                    .map(|p| p.to_string_lossy().to_string())
            })
            .collect::<VendorResult<_>>()?;
        // Await each attempt's cleanup before retrying. The core retry helper's
        // select drops a vendor body, which would detach file completion here.
        for attempt in 0..3 {
            let result = async {
                if let Some(reason) = context.cancellation.reason() {
                    return Err(VendorError::Cancelled(reason));
                }
                let response = device
                    .safe_execute_shell_command("pm list packages", "", context.clone())
                    .await?
                    .ok_or_else(|| {
                        setup(format!("Unable to verify that package {} is installed", package.to_owned()).as_str())
                    })?;
                if jcompat::jstring::lines(&response.output)
                    .iter()
                    .any(|line| *line == format!("package:{package}"))
                {
                    self.log(
                        Level::INFO,
                        &format!(
                            "Uninstalling {} from {}",
                            package.to_owned(),
                            device.serial_number().to_owned()
                        ),
                        None,
                    );
                    if let Some(r) = device.safe_uninstall_package(package, false, context.clone()).await? {
                        self.log(Level::DEBUG, &trim(&r.output.to_owned()), None);
                    }
                }
                self.log(
                    Level::INFO,
                    &format!(
                        "Installing {}, {} to {}",
                        package.to_owned(),
                        paths.join(", "),
                        device.serial_number().to_owned()
                    ),
                    None,
                );
                let params = self.optional_params(device.api_level());
                let output = if splits.is_empty() {
                    device
                        .install_package(&paths[0], true, &params, context.clone())
                        .await?
                        .output
                } else {
                    device
                        .install_split_packages(&paths, true, &params, context.clone())
                        .await?
                };
                let output = trim(&output.to_owned());
                self.log(Level::DEBUG, &output, None);
                if !output.contains("Success") {
                    return Err(install(output.as_str().to_owned()));
                }
                Ok(())
            }
            .await;
            let result = result.map_err(|e| {
                if e.failure().kind == FailureKind::Android(AndroidKind::Install) {
                    self.log(
                        Level::ERROR,
                        &format!(
                            "Error while installing {}, {} on {}",
                            package.to_owned(),
                            paths.join(", "),
                            device.serial_number().to_owned()
                        ),
                        Some(e.failure()),
                    );
                    let mut f = failure(
                        FailureKind::DeviceSetup,
                        format!(
                            "Error while installing {} on {}",
                            package.to_owned(),
                            device.serial_number().to_owned()
                        )
                        .as_str(),
                    );
                    f.cause = Some(Box::new(e.failure().clone()));
                    VendorError::DeviceSetup(f)
                } else {
                    e
                }
            });
            match result {
                Ok(()) => return Ok(()),
                Err(e @ VendorError::Cancelled(_)) => return Err(e),
                Err(e) if attempt == 2 => return Err(e),
                Err(_) => context.clock.delay(1000, context.cancellation.clone()).await?,
            }
        }
        unreachable!("last attempt returns")
    }
    pub fn optional_params(&self, api: i32) -> Vec<String> {
        let mut result = vec![];
        if api >= 23 && self.runtime.configuration.auto_grant_permission {
            result.push("-g".into())
        }
        let options = &self.runtime.configuration.install_options;
        if !options.is_empty() {
            result.extend(options.split(' ').map(str::to_owned));
        }
        result
    }
}
