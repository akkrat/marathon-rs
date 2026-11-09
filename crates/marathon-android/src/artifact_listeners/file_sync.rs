//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/executor/listeners/filesync/FileSyncTestRunListener.kt
use super::*;
use crate::{listeners::AndroidTestRunListener, remote_file_manager::TMP_PATH};
use jcompat::logging::Level;
use marathon_config::vendor::{AggregationMode, PathRoot};
use marathon_core::{runtime::VendorFuture, vendor::VendorContext};
use marathon_dex::apk_parser::InstrumentationInfo;
use std::path::PathBuf;
pub struct FileSyncTestRunListener {
    pub artifacts: ArtifactContext,
    /// The batch's application packages, one per instrumentation in the usual case of one.
    packages: Mutex<Vec<String>>,
}
impl FileSyncTestRunListener {
    pub fn new(artifacts: ArtifactContext) -> Self {
        Self {
            artifacts,
            packages: Mutex::default(),
        }
    }
    fn path(&self, root: PathRoot, relative: &str) -> String {
        join(
            if root == PathRoot::ExternalStorage {
                self.artifacts.device.external_storage_mount().to_owned()
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
            let package = info.map(|i| i.application_package.to_owned());
            *lock(&self.packages) = package.iter().cloned().collect();
            // The provider owns the effective user + Allure + coverage set (6.5 P3).
            for entry in &device.runtime().file_sync.pull {
                let relative = entry.relative_path.to_owned();
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
                let local = jcompat::file_manager::file_path(&entry.path.to_owned());
                let name = basename(&entry.path);
                let remote = self.path(entry.path_root, &name.to_owned());
                let path = local.clone();
                let directory = blocking(device.runtime().dependencies.io.as_ref(), move || Ok(path.is_dir())).await?;
                let local = local.to_string_lossy().into_owned().to_owned();
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
                    // D27: nothing else clears this any more. A leftover from a run cut short would stay what the tests
                    // read, with this push one level inside it.
                    device
                        .file_manager()
                        .remove_push_destination(&remote, context.clone())
                        .await?;
                    device
                        .push_folder(&entry.path.to_owned(), &remote, context.clone())
                        .await?;
                } else {
                    device.push_file(&local, &remote, true, context.clone()).await?;
                }
            }
            Ok(())
        })
    }
    /// D28: the storage is shared and was prepared once, and the one afterTestRun pulls it after every instrumentation
    /// ran. Only this instrumentation's app data remains to prepare, and to be collected at the end.
    fn before_next_instrumentation_in<'a>(
        &'a self,
        info: &'a InstrumentationInfo,
        context: VendorContext,
    ) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let device = &self.artifacts.device;
            let package = info.application_package.to_owned();
            {
                let mut packages = lock(&self.packages);
                if packages.contains(&package) {
                    return Ok(());
                }
                packages.push(package.clone());
            }
            for entry in &device.runtime().file_sync.pull {
                if entry.path_root == PathRoot::AppData {
                    let from = join(&format!("/data/data/{package}"), &entry.relative_path.to_owned());
                    device
                        .safe_execute_shell_command(&format!("run-as {package} rm -R {from}"), "", context.clone())
                        .await?;
                    device
                        .safe_execute_shell_command(&format!("run-as {package} mkdir {from}"), "", context.clone())
                        .await?;
                }
            }
            Ok(())
        })
    }
    fn after_test_run_in(&self, context: VendorContext) -> VendorFuture<'_, ()> {
        Box::pin(async move {
            let device = &self.artifacts.device;
            let packages = lock(&self.packages).clone();
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
                let relative = entry.relative_path.to_owned();
                let base = relative
                    .strip_suffix('/')
                    .unwrap_or(&relative)
                    .rsplit('/')
                    .next()
                    .unwrap_or("");
                let local = jcompat::file_manager::child_file(&folder, base);
                let path = local.clone();
                let local = blocking(device.runtime().dependencies.io.as_ref(), move || {
                    let _ = std::fs::create_dir_all(&path);
                    let cwd = std::env::current_dir().map_err(io)?;
                    let cwd = PathBuf::from(cwd.to_string_lossy().into_owned());
                    let absolute =
                        jcompat::file_manager::absolute_file_path(&local.to_string_lossy(), &cwd.to_string_lossy());
                    Ok(absolute.to_string_lossy().into_owned().to_owned())
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
                    if !packages.is_empty() {
                        for package in &packages {
                            let from = join(&format!("/data/data/{package}"), &relative);
                            device.safe_execute_shell_command(&format!("mkdir -p {remote} && run-as {package} sh -c 'cd {from} && tar cf - .' | tar xvf - -C {remote} && run-as {package} rm -R {from}"),"",context.clone()).await?;
                        }
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
                        .remove_pushed_path(&self.path(entry.path_root, &name.to_owned()), context.clone())
                        .await?;
                }
            }
            lock(&self.packages).clear();
            Ok(())
        })
    }
}

fn basename(path: &str) -> String {
    let path = jcompat::file_manager::file_path(path);
    path.to_string_lossy().rsplit('/').next().unwrap_or("").to_owned()
}
