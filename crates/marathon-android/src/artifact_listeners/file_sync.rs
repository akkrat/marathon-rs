//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/filesync/FileSyncTestRunListener.kt
use super::*;
use crate::{listeners::AndroidTestRunListener, remote_file_manager::TMP_PATH};
use jcompat::logging::Level;
use marathon_config::vendor::{AggregationMode, PathRoot};
use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
use marathon_dex::apk_parser::InstrumentationInfo;
pub struct FileSyncTestRunListener {
    pub artifacts: ArtifactContext,
    package: Mutex<Option<String>>,
}
impl FileSyncTestRunListener {
    pub fn new(artifacts: ArtifactContext) -> Self {
        Self {
            artifacts,
            package: Mutex::new(None),
        }
    }
    fn path(&self, root: PathRoot, relative: &str) -> String {
        join(
            if root == PathRoot::ExternalStorage {
                self.artifacts.device.external_storage_mount().to_string_lossy()
            } else {
                TMP_PATH.into()
            }
            .as_str(),
            relative,
        )
    }
}
fn join(base: &str, relative: &str) -> String {
    format!(
        "{}/{}",
        base.strip_suffix('/').unwrap_or(base),
        relative.strip_prefix('/').unwrap_or(relative)
    )
}
impl AndroidTestRunListener for FileSyncTestRunListener {
    fn before_test_run_in<'a>(
        &'a self,
        info: Option<&'a InstrumentationInfo>,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = &self.artifacts.device;
            let package = info.map(|i| i.application_package.to_string_lossy());
            *lock(&self.package) = package.clone();
            // The provider owns the effective user + Allure + coverage set (6.5 P3).
            for entry in &device.runtime().file_sync.pull {
                let relative = entry.relative_path.to_string_lossy();
                if entry.path_root == PathRoot::AppData && package.is_none() {
                    continue;
                }
                let path = self.path(entry.path_root, &relative);
                device
                    .file_manager()
                    .remove_remote_path(&path, true, context.clone())
                    .await?;
                device
                    .file_manager()
                    .create_remote_directory(Some(&path), context.clone())
                    .await?;
                if entry.path_root == PathRoot::AppData
                    && let Some(package) = &package
                {
                    let from = join(&format!("/data/data/{package}"), &relative);
                    device
                        .safe_execute_shell_command(&format!("run-as {package} rm -R {from}"), "", context.clone())
                        .await?;
                    device
                        .safe_execute_shell_command(&format!("run-as {package} mkdir {from}"), "", context.clone())
                        .await?;
                }
            }
            for entry in &device.runtime().file_sync.push {
                if entry.path_root == PathRoot::AppData {
                    device.log_named(
                        "FileSyncTestRunListener",
                        Level::Error,
                        "Pushing files to app storage is not supported",
                        None,
                    );
                    continue;
                }
                let local = jcompat::file_manager::file_path(&entry.path.to_string_lossy());
                let name = basename(&entry.path);
                let remote = self.path(entry.path_root, &name.to_string_lossy());
                let path = local.to_file_path_buf().map_err(io)?;
                let directory = blocking(device.runtime().dependencies.io.as_ref(), move || Ok(path.is_dir())).await?;
                let local = JavaString(local.0.clone()).to_string_lossy();
                device.log_named(
                    "FileSyncTestRunListener",
                    Level::Debug,
                    &format!(
                        "Pushing {} {local} into {remote}",
                        if directory { "folder" } else { "file" }
                    ),
                    None,
                );
                if directory {
                    device
                        .push_folder(&entry.path.to_string_lossy(), &remote, context.clone())
                        .await?;
                } else {
                    device.push_file(&local, &remote, true, context.clone()).await?;
                }
            }
            Ok(())
        })
    }
    fn after_test_run_in(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let device = &self.artifacts.device;
            let package = lock(&self.package).clone();
            for entry in &device.runtime().file_sync.pull {
                let pool = matches!(
                    entry.aggregation_mode,
                    AggregationMode::DeviceAndPool | AggregationMode::Pool
                )
                .then_some(&self.artifacts.pool);
                let info = device.info();
                let serial = matches!(
                    entry.aggregation_mode,
                    AggregationMode::DeviceAndPool | AggregationMode::Device
                )
                .then_some(&info);
                let folder = device
                    .runtime()
                    .dependencies
                    .files
                    .create_folder(FolderType::DeviceFiles, pool, serial)
                    .await
                    .map_err(operation_error)?;
                let relative = entry.relative_path.to_string_lossy();
                let base = relative
                    .strip_suffix('/')
                    .unwrap_or(&relative)
                    .rsplit('/')
                    .next()
                    .unwrap_or("");
                let local = jcompat::file_manager::child_file(&folder, &base.encode_utf16().collect::<Vec<_>>());
                let path = local.to_file_path_buf().map_err(io)?;
                let local = blocking(device.runtime().dependencies.io.as_ref(), move || {
                    let _ = std::fs::create_dir_all(&path);
                    let cwd = std::env::current_dir().map_err(io)?;
                    let cwd = JavaPath::from_text(&cwd.to_string_lossy());
                    let absolute = jcompat::file_manager::absolute_file_path_utf16(&local.0, &cwd.0);
                    Ok(JavaString(absolute.0).to_string_lossy())
                })
                .await?;
                device.log_named(
                    "FileSyncTestRunListener",
                    Level::Debug,
                    &format!("Pulling into {local}"),
                    None,
                );
                let remote = self.path(entry.path_root, &relative);
                if entry.path_root == PathRoot::AppData {
                    if let Some(package) = &package {
                        let from = join(&format!("/data/data/{package}"), &relative);
                        device.safe_execute_shell_command(&format!("mkdir -p {remote} && run-as {package} sh -c 'cd {from} && tar cf - .' | tar xvf - -C {remote} && run-as {package} rm -R {from}"),"",context.clone()).await?;
                    } else {
                        let aggregation = match entry.aggregation_mode {
                            AggregationMode::Device => "DEVICE",
                            AggregationMode::Pool => "POOL",
                            AggregationMode::DeviceAndPool => "DEVICE_AND_POOL",
                            AggregationMode::TestRun => "TEST_RUN",
                        };
                        device.log_named("FileSyncTestRunListener",Level::Warn,&format!("Unable to pull FileSyncEntry(relativePath={relative}, pathRoot=APP_DATA, aggregationMode={aggregation}): application package is unknown"),None);
                        continue;
                    }
                }
                device.safe_pull_folder(&remote, &local, context.clone()).await?;
            }
            for entry in &device.runtime().file_sync.push {
                if entry.path_root != PathRoot::AppData {
                    let name = basename(&entry.path);
                    device
                        .file_manager()
                        .remove_remote_path(
                            &self.path(entry.path_root, &name.to_string_lossy()),
                            true,
                            context.clone(),
                        )
                        .await?;
                }
            }
            *lock(&self.package) = None;
            Ok(())
        })
    }
}

fn basename(path: &JavaString) -> JavaString {
    let path = jcompat::file_manager::file_path(&path.to_string_lossy());
    JavaString(path.0.rsplit(|u| *u == 47).next().unwrap_or(&[]).to_vec())
}
