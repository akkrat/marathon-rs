//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/xctestrun/TestRootFactory.kt

use super::v2::{ArtifactReinstallParams, Metadata, TestConfiguration, TestTarget, Xctestrun};
use crate::{
    apple_device::AppleDevice,
    bin::kt,
    cmd::{blocking, failure},
    distributed_derived_data::require_distributed,
    logparser::parser::DeviceFailureReason,
    model::{AppleOperatingSystemVersion, AppleTestBundle, Arch, Sdk},
    plist::{PlistError, PropertyList, bundle::BundleInfo, delegates::OrderedStringMap},
};
use jcompat::file_manager::relative_file;
use marathon_config::{
    CONFIGURATION_EXCEPTION_CLASS,
    vendor::{TestType, XcresultConfiguration, XctestrunEnvConfiguration},
};
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    runtime::VendorRuntime,
    vendor::VendorContext,
};
use marathon_error::{AppleKind, FailureKind, FatalKind};
use std::path::PathBuf;
use std::{path::Path, sync::Arc};

const INCOMPATIBLE: FailureKind = FailureKind::IncompatibleDevice;
const SETUP: FailureKind = FailureKind::DeviceSetup;
const TRANSFER: FailureKind = FailureKind::Transfer;
const DEVICE_FAILURE: FailureKind = FailureKind::Apple(AppleKind::DeviceFailure);
const DYLD: [&str; 3] = ["DYLD_FRAMEWORK_PATH", "DYLD_LIBRARY_PATH", "DYLD_INSERT_LIBRARIES"];

/// Installer/simulator callers inspect DeviceFailure's reason before crossing the core bridge.
/// It is not an IncompatibleDeviceException: Kotlin's catches distinguish those classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactoryError {
    DeviceFailure {
        reason: DeviceFailureReason,
        detail: Failure,
    },
    Vendor(VendorError),
}
impl FactoryError {
    fn from_plist(path: &Path, error: PlistError) -> Self {
        crate::plist::plist_failure(path, error).into()
    }
    pub fn failure(&self) -> &Failure {
        match self {
            Self::DeviceFailure { detail, .. } => detail,
            Self::Vendor(error) => error.failure(),
        }
    }
    /// Use only after vendor-local reason handling (4.10b/5.2); core cannot carry that reason.
    pub fn into_vendor(self) -> VendorError {
        match self {
            Self::DeviceFailure { detail, .. } => VendorError::Other(detail),
            Self::Vendor(error) => error,
        }
    }
}
impl From<VendorError> for FactoryError {
    fn from(error: VendorError) -> Self {
        let error = match error {
            VendorError::Other(detail) if detail.kind == SETUP => VendorError::DeviceSetup(detail),
            VendorError::Other(detail) if detail.kind == INCOMPATIBLE => VendorError::IncompatibleDevice(detail),
            VendorError::Other(detail) if detail.kind == TRANSFER => VendorError::Transfer(detail),
            other => other,
        };
        Self::Vendor(error)
    }
}
impl From<PlistError> for FactoryError {
    fn from(error: PlistError) -> Self {
        Self::from_plist(Path::new(""), error)
    }
}
pub type FactoryResult<T> = Result<T, FactoryError>;
fn configuration(message: &str) -> FactoryError {
    VendorError::Other(failure(CONFIGURATION_EXCEPTION_CLASS, message)).into()
}
fn setup(message: String) -> VendorError {
    let mut detail = failure(SETUP, "");
    detail.message = Some(message);
    VendorError::DeviceSetup(detail)
}
fn io_error(error: std::io::Error) -> VendorError {
    VendorError::Other(failure(FailureKind::Io, error.to_string()))
}
fn path_text(path: &Path) -> VendorResult<&str> {
    path.to_str()
        .ok_or_else(|| VendorError::Other(failure(FailureKind::InvalidPath, "path is not valid UTF-8")))
}
fn name(path: &Path) -> VendorResult<&str> {
    Ok(path_text(path)?.rsplit('/').next().unwrap_or(""))
}
fn stem(path: &Path) -> VendorResult<&str> {
    let name = name(path)?;
    Ok(name.rfind('.').map_or(name, |dot| &name[..dot]))
}
fn put(map: &mut OrderedStringMap, key: &str, value: String) {
    if let Some((_, old)) = map.iter_mut().find(|(k, _)| k == key) {
        *old = value;
    } else {
        map.push((key.into(), value));
    }
}

pub struct TestRootFactory<'a> {
    device: &'a dyn AppleDevice,
    env: &'a XctestrunEnvConfiguration,
    xcresult: &'a XcresultConfiguration,
    skip_copying_derived_data: bool,
    runtime: Arc<dyn VendorRuntime>,
    code_coverage: Option<std::path::PathBuf>,
}
impl<'a> TestRootFactory<'a> {
    pub fn new(
        device: &'a dyn AppleDevice,
        env: &'a XctestrunEnvConfiguration,
        xcresult: &'a XcresultConfiguration,
        skip_copying_derived_data: bool,
        runtime: Arc<dyn VendorRuntime>,
    ) -> Self {
        Self {
            device,
            env,
            xcresult,
            skip_copying_derived_data,
            runtime,
            code_coverage: None,
        }
    }
    pub fn with_code_coverage(mut self, xctestrun: Option<std::path::PathBuf>) -> Self {
        self.code_coverage = xctestrun;
        self
    }
    /// Must be awaited through completion on the vendor runtime, including cancellation cleanup.
    pub async fn generate(
        &self,
        test_type: TestType,
        bundle: &AppleTestBundle,
        use_xctest_parser: bool,
        context: &VendorContext,
    ) -> FactoryResult<()> {
        self.device
            .remote_file_manager()
            .create_remote_directory(Some(self.device.remote_file_manager().remote_test_root()), context)
            .await?;
        if let Some(info) = bundle.application_bundle_info() {
            self.validate(info)?;
        }
        self.validate(bundle.test_bundle_info())?;
        let run = match test_type {
            TestType::Xcuitest => self.ui(bundle, use_xctest_parser, context).await?,
            TestType::Xctest => self.hosted(bundle, use_xctest_parser, context).await?,
            TestType::LogicTest => return Err(Self::todo(None)),
        };
        self.stage(run, context).await
    }
    fn validate(&self, bundle: &BundleInfo) -> FactoryResult<()> {
        let platforms = bundle.undocumented().bundle_supported_platforms()?;
        let info = self.device.info();
        if !platforms.iter().any(|p| p == self.device.sdk().platform_name()) {
            let mut message = String::from("Device ");
            message.push_str(&info.serial_number);
            message.push_str(&format!(
                " with platform {} is incompatible with bundle's supported platforms [{}]",
                self.device.sdk().platform_name(),
                platforms.join(",")
            ));
            let mut detail = failure(DEVICE_FAILURE, "");
            detail.message = Some(message);
            return Err(FactoryError::DeviceFailure {
                reason: DeviceFailureReason::IncompatibleDevice,
                detail,
            });
        }
        if self.device.sdk() == Sdk::Macos {
            // A28: the required delegate throws when the key is absent, before the nullable check.
            let minimum = bundle.operating_system_version().ls_minimum_system_version()?;
            let device_version = info.operating_system.version.clone();
            let bundle_version = AppleOperatingSystemVersion::new(&minimum)?;
            if AppleOperatingSystemVersion::new(&device_version)? < bundle_version {
                let mut message = String::from("Device ");
                message.push_str(&info.serial_number);
                message.push_str(" with os version ");
                message.push_str(&info.operating_system.version);
                message.push_str(&format!(
                    " is incompatible with bundle's minimum required version {minimum}"
                ));
                let mut detail = failure(INCOMPATIBLE, "");
                detail.message = Some(message);
                return Err(VendorError::IncompatibleDevice(detail).into());
            }
        }
        Ok(())
    }
    fn todo(message: Option<&str>) -> FactoryError {
        VendorError::Other(failure(
            FailureKind::Fatal(FatalKind::NotImplemented),
            message.map_or("An operation is not implemented.".into(), |m| {
                format!("An operation is not implemented: {m}")
            }),
        ))
        .into()
    }
    async fn stage(&self, run: Xctestrun, context: &VendorContext) -> FactoryResult<()> {
        stage_file(
            &*self.runtime,
            self.device.udid().to_owned(),
            move |path| {
                run.save_as_xml_to_file(path)
                    .map_err(|e| FactoryError::from_plist(path, e).into_vendor())
            },
            |path| async move {
                if !self
                    .device
                    .push_generated_file(
                        &path,
                        &self.device.remote_file_manager().remote_xctestrun_file(),
                        context,
                    )
                    .await?
                {
                    let mut message = String::from("Unable to transfer generated xctestrun file to ");
                    message.push_str(&self.device.info().serial_number);
                    let mut detail = failure(TRANSFER, "");
                    detail.message = Some(message);
                    return Err(VendorError::Transfer(detail));
                }
                Ok(())
            },
        )
        .await
        .map_err(FactoryError::from)
    }
    fn join(&self, base: &str, args: &[&str]) -> String {
        self.device.remote_file_manager().join_path(base, args)
    }
    fn binary_path(&self, bundle: &AppleTestBundle, base: &str, file: &str) -> String {
        let mut path = bundle.relative_binary_path();
        path.push(file);
        self.join(base, &path)
    }
    fn framework_path(&self, bundle: &AppleTestBundle, base: &str) -> String {
        let mut path = bundle.relative_root_path();
        path.push("Frameworks");
        self.join(base, &path)
    }
    async fn application(
        &self,
        bundle: &AppleTestBundle,
        kind: &str,
        context: &VendorContext,
    ) -> FactoryResult<String> {
        let app = bundle
            .application
            .as_ref()
            .ok_or_else(|| configuration(&format!("no application specified for {kind}")))?;
        let remote = self.device.remote_file_manager().remote_application();
        if self.skip_copying_derived_data {
            let binary = bundle.application_binary().await?;
            let required = match binary {
                Some(ref binary) => self.binary_path(bundle, &remote, name(binary)?),
                None => remote.clone(),
            };
            require_distributed(self.device, app, &remote, &required, setup, context).await?;
        } else if !self.device.push_shared(app, &remote, context).await? {
            return Err(setup("failed to push app under test to remote device".into()).into());
        }
        if let Some(binary) = bundle.application_binary().await? {
            self.device
                .binary_environment()
                .chmod
                .make_executable(&self.binary_path(bundle, &remote, name(&binary)?), context)
                .await?;
        }
        Ok(remote)
    }
    async fn runner(
        &self,
        bundle: &AppleTestBundle,
        library: &str,
        context: &VendorContext,
    ) -> FactoryResult<(String, String)> {
        let fm = self.device.remote_file_manager();
        let binary_name = format!("{}-Runner", bundle.test_bundle_id());
        let runner = self.join(fm.remote_test_root(), &[&format!("{binary_name}.app")]);
        if let Some(local_runner) = &bundle.test_application {
            let binary = bundle.test_runner_binary().await?;
            let runner_binary = self.binary_path(bundle, &fm.remote_test_runner_application(), name(&binary)?);
            self.device
                .binary_environment()
                .chmod
                .make_executable(&runner_binary, context)
                .await?;
            fm.symlink(&fm.remote_test_runner_application(), &runner, true, context)
                .await?;
            let local = PathBuf::from(path_text(&bundle.xctest_bundle)?);
            let base = PathBuf::from(path_text(local_runner)?);
            let relative = blocking(&*self.runtime, move || {
                let cwd = std::env::current_dir().map_err(io_error)?;
                Ok(relative_file(&local, &base, &PathBuf::from(path_text(&cwd)?)))
            })
            .await?;
            let remote_test = self.join(&runner, &[&relative]);
            let test_binary = bundle.test_binary().await?;
            self.match_architectures(
                &self.binary_path(bundle, &remote_test, stem(&test_binary)?),
                &runner_binary,
                context,
            )
            .await?;
            Ok((runner, remote_test))
        } else {
            let test_binary = bundle.test_binary().await?;
            let remote_test = fm.remote_xctest_file();
            let remote_binary = self.binary_path(bundle, &remote_test, stem(&test_binary)?);
            fm.copy(
                &self.join(library, &["Xcode", "Agents", "XCTRunner.app"]),
                &runner,
                true,
                context,
            )
            .await?;
            let runner_binary = self.binary_path(bundle, &runner, &binary_name);
            fm.copy(
                &self.binary_path(bundle, &runner, "XCTRunner"),
                &runner_binary,
                true,
                context,
            )
            .await?;
            self.match_architectures(&remote_binary, &runner_binary, context)
                .await?;
            let plist = self.join(
                &runner,
                if self.device.sdk() == Sdk::Macos {
                    &["Contents", "Info.plist"]
                } else {
                    &["Info.plist"]
                },
            );
            for (key, value) in [
                ("CFBundleName", binary_name.clone()),
                ("CFBundleExecutable", binary_name.clone()),
                ("CFBundleIdentifier", format!("com.apple.test.{binary_name}")),
            ] {
                let _ = self
                    .device
                    .binary_environment()
                    .plist_buddy
                    .set(&plist, key, &value, context)
                    .await?;
            }
            if self.device.sdk() == Sdk::Macos {
                let plugins = self.join(&runner, &["Contents", "PlugIns"]);
                fm.create_remote_directory(Some(&plugins), context).await?;
                fm.copy(&remote_test, &plugins, false, context).await?;
            }
            Ok((runner, remote_test))
        }
    }
    async fn match_architectures(&self, test: &str, runner: &str, context: &VendorContext) -> VendorResult<()> {
        let arch = self.device.arch();
        if arch == Arch::arm64e || self.device.sdk() == Sdk::IphoneSimulator {
            let lipo = &self.device.binary_environment().lipo;
            let test_archs = lipo.get_arch(test, context).await?;
            let runner_archs = lipo.get_arch(runner, context).await?;
            let remove = if arch == Arch::arm64e {
                (!test_archs.contains(&Arch::arm64e) && runner_archs.contains(&Arch::arm64e)).then_some(Arch::arm64e)
            } else {
                (test_archs.contains(&Arch::x86_64)
                    && runner_archs.contains(&Arch::arm64)
                    && (arch != Arch::arm64 || !test_archs.contains(&Arch::arm64)))
                .then_some(Arch::arm64)
            };
            if let Some(remove) = remove {
                lipo.remove_arch(runner, remove, context).await?;
            }
        }
        Ok(())
    }
    fn user_paths(user: &OrderedStringMap, key: &str) -> Vec<String> {
        user.iter().find(|(k, _)| k == key).map_or_else(Vec::new, |(_, v)| {
            v.split(':').filter(|s| !kt::is_blank(s)).map(str::to_owned).collect()
        })
    }
    async fn target(
        &self,
        bundle: &AppleTestBundle,
        mut params: ArtifactReinstallParams,
        mut env: OrderedStringMap,
        user: OrderedStringMap,
        roots: Vec<(std::path::PathBuf, String)>,
        context: &VendorContext,
    ) -> FactoryResult<Xctestrun> {
        for (key, value) in user {
            if !DYLD.contains(&key.as_str()) {
                put(&mut env, &key, value);
            }
        }
        params.testing_environment_variables = env;
        if !self.env.app_envs.is_empty() {
            params.environment_variables = Some(self.env.app_envs.clone());
        }
        params.product_module_name = Some(bundle.test_bundle_id().into());
        params.system_attachment_lifetime = Some(self.xcresult.attachments.system_attachment_lifetime.value().into());
        params.user_attachment_lifetime = Some(self.xcresult.attachments.user_attachment_lifetime.value().into());
        let target = TestTarget::with_artifact_reinstall(params);
        let coverage = if let Some(path) = &self.code_coverage {
            let path = path.clone();
            let fm = self.device.remote_file_manager().clone();
            let static_directory = self.join(fm.remote_shared_directory(), &["coverage"]);
            Some(
                blocking(&*self.runtime, move || {
                    super::code_coverage_targets::to_remote(&path, &roots, &static_directory, |base, parts| {
                        fm.join_path(base, parts)
                    })
                })
                .await?,
            )
        } else {
            None
        };
        if let Some(coverage) = &coverage {
            let fm = self.device.remote_file_manager();
            for (local, remote) in &coverage.static_libraries {
                fm.create_remote_directory(Some(fm.parent_of(remote)), context).await?;
                if !self.device.push_shared(local, remote, context).await? {
                    return Err(setup(format!(
                        "failed to push static library {} for code coverage to remote device",
                        local.display()
                    ))
                    .into());
                }
            }
            target.set_clang_profile_data_directory_path(Some(self.join(fm.remote_directory(), &["ProfileData"])));
        }
        Ok(Xctestrun::new(
            &Metadata::new(2),
            &[TestConfiguration::new("marathon".into(), &[target])],
            None,
            coverage.as_ref().map(|c| c.infos.as_slice()),
        ))
    }

    async fn ui(&self, bundle: &AppleTestBundle, parser: bool, context: &VendorContext) -> FactoryResult<Xctestrun> {
        let platform = self
            .device
            .binary_environment()
            .xcrun
            .get_sdk_platform_path(self.device.sdk(), context)
            .await?;
        let (runner, test) = self
            .runner(bundle, &self.join(&platform, &["Developer", "Library"]), context)
            .await?;
        let app = self.application(bundle, "XCUITest", context).await?;
        if self.device.sdk() == Sdk::Iphoneos {
            return Err(Self::todo(Some("generate phone provisioning")));
        }
        let developer = format!("__PLATFORMS__/{}.platform/Developer", self.device.sdk().platform_name());
        let user = self.env.test_envs.clone();
        let mut frameworks = vec![
            "__TESTROOT__".into(),
            self.framework_path(bundle, &app),
            self.framework_path(bundle, &runner),
            format!("{developer}/Library/Frameworks"),
            format!("{developer}/Library/PrivateFrameworks"),
        ];
        frameworks.extend(Self::user_paths(&user, DYLD[0]));
        let mut libraries = vec!["__TESTROOT__".into()];
        if let Some(local) = &bundle.test_application {
            let dir = bundle
                .relative_frameworks_path()
                .iter()
                .fold(local.clone(), |p, c| p.join(c));
            let dylibs = blocking(&*self.runtime, move || {
                Ok(std::fs::read_dir(dir)
                    .ok()
                    .and_then(|entries| entries.collect::<Result<Vec<_>, _>>().ok())
                    .is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry.file_name().to_str().is_some_and(|name| {
                                name.rsplit_once('.').is_some_and(|(_, extension)| extension == "dylib")
                            })
                        })
                    }))
            })
            .await?;
            if dylibs {
                libraries.push(self.framework_path(bundle, &runner));
            }
        }
        libraries.push(format!("{developer}/usr/lib"));
        libraries.extend(Self::user_paths(&user, DYLD[1]));
        let mut inserts = vec![];
        if parser {
            inserts.push(self.device.remote_file_manager().remote_xctest_parser_file());
        }
        inserts.extend(Self::user_paths(&user, DYLD[2]));
        let env = vec![
            (DYLD[0].into(), frameworks.join(":")),
            (DYLD[1].into(), libraries.join(":")),
            (DYLD[2].into(), inserts.join(":")),
        ];
        let mut params =
            ArtifactReinstallParams::new(bundle.test_bundle_id().into(), test.clone(), runner.clone(), vec![]);
        params.ui_target_app_path = Some(app.clone());
        params.dependent_product_paths = Some(vec![runner.clone(), test.clone(), app.clone()]);
        let mut roots = vec![
            (
                bundle
                    .application
                    .clone()
                    .ok_or_else(|| configuration("no application specified for XCUITest"))?,
                app,
            ),
            (bundle.xctest_bundle.clone(), test),
        ];
        if let Some(local) = &bundle.test_application {
            roots.push((local.clone(), runner));
        }
        params.is_ui_test_bundle = Some(true);
        params.preferred_screen_capture_format = self
            .xcresult
            .preferred_screen_capture_format
            .map(|format| format.xcode_value().into());
        self.target(bundle, params, env, user, roots, context).await
    }
    async fn hosted(
        &self,
        bundle: &AppleTestBundle,
        parser: bool,
        context: &VendorContext,
    ) -> FactoryResult<Xctestrun> {
        let app = self.application(bundle, "XCTest", context).await?;
        let test = self.join(&app, &["PlugIns", name(&bundle.xctest_bundle)?]);
        if self.skip_copying_derived_data {
            let binary = bundle.test_binary().await?;
            require_distributed(
                self.device,
                &bundle.xctest_bundle,
                &test,
                &self.binary_path(bundle, &test, name(&binary)?),
                setup,
                context,
            )
            .await?;
        } else {
            self.device
                .remote_file_manager()
                .create_remote_directory(Some(&self.join(&app, &["PlugIns"])), context)
                .await?;
            // A9: Kotlin compares File to Path, so it always performs this push.
            if !self.device.push_shared(&bundle.xctest_bundle, &test, context).await? {
                return Err(setup("failed to push xctest to remote device".into()).into());
            }
        }
        if self.device.sdk() == Sdk::Iphoneos {
            return Err(Self::todo(Some("generate phone provisioning")));
        }
        let developer = format!("__PLATFORMS__/{}.platform/Developer", self.device.sdk().platform_name());
        let user = self.env.test_envs.clone();
        let mut frameworks = vec![
            "__TESTROOT__".into(),
            format!("{developer}/Library/Frameworks"),
            format!("{developer}/Library/PrivateFrameworks"),
        ];
        frameworks.extend(Self::user_paths(&user, DYLD[0]));
        if let Some(local) = &bundle.application
            && tokio::fs::metadata(local.join("Frameworks")).await.is_ok()
        {
            frameworks.push("__TESTROOT__/appUnderTest.app/Frameworks".into());
        }
        let mut libraries = vec!["__TESTROOT__".into(), format!("{developer}/usr/lib")];
        libraries.extend(Self::user_paths(&user, DYLD[1]));
        let mut inserts = vec![];
        if parser {
            inserts.push(self.device.remote_file_manager().remote_xctest_parser_file());
        }
        inserts.push(format!("{developer}/usr/lib/libXCTestBundleInject.dylib"));
        inserts.extend(Self::user_paths(&user, DYLD[2]));
        let env = vec![
            (DYLD[0].into(), frameworks.join(":")),
            (DYLD[2].into(), inserts.join(":")),
            (DYLD[1].into(), libraries.join(":")),
            ("XCInjectBundleInto".into(), "__TESTHOST__/appUnderTest.app".into()),
        ];
        let roots = vec![
            (
                bundle
                    .application
                    .clone()
                    .ok_or_else(|| configuration("no application specified for XCTest"))?,
                app.clone(),
            ),
            (bundle.xctest_bundle.clone(), test.clone()),
        ];
        let mut params = ArtifactReinstallParams::new(bundle.test_bundle_id().into(), test, app, vec![]);
        params.is_app_hosted_test_bundle = Some(true);
        self.target(bundle, params, env, user, roots, context).await
    }
}

// The writer seam allows deterministic write-failure tests without a filesystem race.
async fn stage_file<T, F: std::future::Future<Output = VendorResult<T>>>(
    runtime: &dyn VendorRuntime,
    prefix: String,
    write: impl FnOnce(&Path) -> VendorResult<()> + Send + 'static,
    send: impl FnOnce(std::path::PathBuf) -> F,
) -> VendorResult<T> {
    let temporary = blocking(runtime, move || {
        // Files.createTempFile rejects separators; tempfile's builder otherwise permits them.
        if prefix.contains('/') {
            return Err(VendorError::Other(failure(
                FailureKind::IllegalArgument,
                "Invalid prefix or suffix",
            )));
        }
        tempfile::Builder::new()
            .prefix(&prefix)
            .suffix("xctestrun")
            .tempfile()
            .map_err(io_error)
    })
    .await?;
    let path = temporary.path().to_owned();
    let result = async {
        blocking(runtime, move || write(&path)).await?;
        send(temporary.path().to_owned()).await
    }
    .await;
    // Cleanup relies on cooperative cancellation (lifecycle.md): dropping this future instead lets
    // NamedTempFile unlink the path while a detached blocking writer may still recreate it.
    // Kotlin ignores File.delete's Boolean; even failed cleanup cannot replace a writer/send error.
    let _ = blocking(runtime, move || {
        let _ = temporary.close();
        Ok(())
    })
    .await;
    result
}

#[cfg(test)]
use crate::test_support as staging_test_support;

#[cfg(test)]
#[tokio::test]
async fn staging_writer_failure_cleans_up_and_preserves_the_error() {
    use std::sync::Mutex;
    let path = Arc::new(Mutex::new(None));
    let recorded = path.clone();
    let rows = include_str!("../../../../fixtures/oracle/test-root/staging-edges.jsonl");
    let row: serde_json::Value = rows
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|row| row["input"]["mode"] == "writer-fail")
        .unwrap();
    assert_eq!(row["cleaned"], true);
    assert!(
        row["pushes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["kind"] != "generated")
    );
    let expected = VendorError::Other(failure(
        marathon_test_support::recorded_errors::failure_kind(
            row["error"]["class"].as_str().unwrap(),
            row["error"]["message"].as_str(),
        )
        .unwrap_or_else(|e| panic!("{e}")),
        row["error"]["message"].as_str().unwrap(),
    ));
    let write_error = expected.clone();
    let result: VendorResult<()> = stage_file(
        &staging_test_support::Runtime,
        "writer-failure".into(),
        move |path| {
            *recorded.lock().unwrap() = Some(path.to_owned());
            Err(write_error)
        },
        |_| async { panic!("writer failure must not send") },
    )
    .await;
    assert_eq!(result, Err(expected));
    let path = path.lock().unwrap().clone().unwrap();
    assert!(!path.exists());
}

#[cfg(test)]
#[tokio::test]
async fn staging_uses_private_temp_file_and_keeps_it_until_send_finishes() {
    use std::{os::unix::fs::PermissionsExt, sync::Mutex};
    let path = Arc::new(Mutex::new(None));
    let recorded = path.clone();
    let result = stage_file(
        &staging_test_support::Runtime,
        "staging-prefix".into(),
        move |path| {
            assert!(
                path.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("staging-prefix")
                        && s.ends_with("xctestrun")
                        && !s.ends_with(".xctestrun"))
            );
            assert_eq!(
                std::fs::metadata(path).map_err(io_error)?.permissions().mode() & 0o777,
                0o600
            );
            *recorded.lock().unwrap() = Some(path.to_owned());
            std::fs::write(path, b"staged bytes").map_err(io_error)
        },
        |path| async move {
            tokio::task::yield_now().await;
            Ok(tokio::fs::read(&path).await.map_err(io_error)? == b"staged bytes")
        },
    )
    .await;
    assert_eq!(result, Ok(true));
    assert!(!path.lock().unwrap().as_ref().unwrap().exists());
}

#[cfg(test)]
mod plist_boundary_test {
    #[test]
    fn factory_plist_error_uses_canonical_class_and_message() -> Result<(), Box<dyn std::error::Error>> {
        let error = super::FactoryError::from(crate::plist::PlistError::Io {
            is_not_found: false,
            message: "read denied".into(),
        });
        assert_eq!(error.failure().kind, marathon_error::FailureKind::Io);
        assert_eq!(error.failure().message.as_ref().unwrap().to_owned(), "read denied");
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("missing.xctestrun");
        let missing = super::FactoryError::from_plist(
            &path,
            <super::Xctestrun as crate::plist::PropertyList>::from_path(&path).unwrap_err(),
        );
        assert_eq!(
            missing.failure().kind.description().to_owned(),
            "no such file or directory"
        );
        assert_eq!(
            missing.failure().message.as_ref().unwrap().to_owned(),
            path.display().to_string()
        );
        Ok(())
    }
}
