//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/RemoteFileManager.kt

use marathon_logging::{Component, Level};

use crate::{bin::kt, cmd::remote::ssh::sshj::config::SshLogging, worker_commands::WorkerCommands};
use jcompat::file_manager::escape;
use marathon_config::vendor::ScreenshotType;
use marathon_core::{
    error::VendorResult,
    model::{Test, TestBatch},
    vendor::VendorContext,
};
use std::sync::Arc;

pub const FILE_SEPARATOR: &str = "/";

/// Paths remain strings: host filesystem normalization would change worker argv.
/// Commands are independent of the device, avoiding an Arc ownership cycle.
#[derive(Clone)]
pub struct RemoteFileManager {
    storage_path: String,
    udid: String,
    commands: Arc<dyn WorkerCommands>,
    logging: SshLogging,
    logger: Component,
}
impl RemoteFileManager {
    /// Construct off core, like the device and its other vendor services.
    pub fn new(storage_path: String, udid: String, commands: Arc<dyn WorkerCommands>, logging: SshLogging) -> Self {
        let logger = logging.component("marathon_apple::remote_file_manager");
        Self {
            storage_path,
            udid,
            commands,
            logging,
            logger,
        }
    }
    pub fn remote_directory(&self) -> &str {
        &self.storage_path
    }
    pub fn remote_test_root(&self) -> &str {
        self.remote_directory()
    }
    pub fn remote_shared_directory(&self) -> &'static str {
        "/tmp/marathon/shared/"
    }
    pub fn remote_shared_media_directory(&self) -> &'static str {
        "/tmp/marathon/shared/mediaFiles/"
    }
    pub fn xctestrun_file_name(&self) -> &'static str {
        "marathon.xctestrun"
    }
    pub fn app_under_test_file_name(&self) -> &'static str {
        "appUnderTest.app"
    }
    pub fn test_runner_file_name(&self) -> &'static str {
        "xctestRunner.app"
    }
    pub fn remote_xctestrun_file(&self) -> String {
        resolve(self.remote_directory(), self.xctestrun_file_name())
    }
    pub fn remote_xctest_file(&self) -> String {
        self.remote_extra_application("marathon.xctest")
    }
    pub fn remote_test_runner_application(&self) -> String {
        self.remote_extra_application(self.test_runner_file_name())
    }
    pub fn remote_xctest_parser_file(&self) -> String {
        self.remote_extra_application("libxctest-parser.dylib")
    }
    pub fn remote_application(&self) -> String {
        self.remote_extra_application(self.app_under_test_file_name())
    }
    pub fn remote_extra_application(&self, name: &str) -> String {
        resolve(self.remote_shared_directory(), name)
    }
    pub fn remote_media_file(&self, file: &str) -> String {
        resolve(self.remote_shared_media_directory(), file)
    }
    pub fn remote_xcresult_file(&self, batch: &TestBatch) -> String {
        self.remote_xcresult_for_id(&batch.id)
    }
    pub fn remote_xcresult_for_id(&self, batch_id: &str) -> String {
        let mut result = String::from(resolve(self.remote_directory(), &format!("{}.", self.udid)).as_str());
        result.push_str(batch_id);
        result.push_str(".xcresult");
        result
    }
    pub fn remote_log(&self) -> String {
        resolve(self.remote_directory(), "device.log")
    }
    pub fn remote_file_for_test(&self, name: &str) -> String {
        format!("{}/{name}", self.storage_path)
    }
    pub fn remote_video_pidfile(&self) -> String {
        self.remote_file_for_test(&format!("{}.pid", self.udid))
    }
    pub fn remote_screenshot(&self, udid: &str, kind: ScreenshotType) -> String {
        self.remote_file_for_test(&format!("{udid}.{}", kind.value()))
    }
    pub fn remote_video_for_test(&self, test: &Test, batch_id: &str) -> String {
        self.video(test, batch_id, false)
    }
    pub fn remote_temp_video_for_test(&self, test: &Test, batch_id: &str) -> String {
        self.video(test, batch_id, true)
    }
    fn video(&self, test: &Test, batch_id: &str, temporary: bool) -> String {
        let mut name = test.to_class_name('-');
        name.push('-');
        name.push_str(&test.method.clone());
        let mut result = String::from(format!("{}/", self.storage_path).as_str());
        result.push_str(&escape(&name));
        result.push('-');
        result.push_str(batch_id);
        result.push_str(if temporary { "-temp.mp4" } else { ".mp4" });
        result
    }
    pub fn parent_of<'a>(&self, path: &'a str) -> &'a str {
        path.rsplit_once('/').map_or(path, |(parent, _)| parent)
    }
    pub fn join_path(&self, base: &str, args: &[&str]) -> String {
        std::iter::once(base.strip_suffix('/').unwrap_or(base))
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join("/")
    }
    pub async fn create_remote_directory(&self, dir: Option<&str>, context: &VendorContext) -> VendorResult<()> {
        let dir = dir.unwrap_or(self.remote_directory());
        self.execute(
            vec!["mkdir".into(), "-p".into(), dir.into()],
            format!("Could not create remote directory {dir}"),
            context,
        )
        .await
    }
    pub async fn create_remote_shared_directory(&self, context: &VendorContext) -> VendorResult<()> {
        self.create_remote_directory(Some(self.remote_shared_directory()), context)
            .await
    }
    pub async fn create_remote_shared_media_directory(&self, context: &VendorContext) -> VendorResult<()> {
        self.create_remote_directory(Some(self.remote_shared_media_directory()), context)
            .await
    }
    pub async fn remove_remote_directory(&self, keep: &[String], context: &VendorContext) -> VendorResult<()> {
        let dir = self.remote_directory();
        let (command, message) = if keep.is_empty() {
            (
                vec!["rm".into(), "-rf".into(), dir.into()],
                format!("Unable to remove directory {dir}"),
            )
        } else {
            let mut command = vec![
                "find".into(),
                dir.into(),
                "-mindepth".into(),
                "1".into(),
                "-maxdepth".into(),
                "1".into(),
            ];
            // Keep Kotlin's glob interpretation and empty basename after '/' (A46).
            for path in keep {
                command.extend([
                    "!".into(),
                    "-name".into(),
                    path.rsplit('/').next().unwrap_or(path).into(),
                ]);
            }
            command.extend(["-exec", "rm", "-rf", "{}", "+"].map(str::to_owned));
            (command, format!("Unable to clean directory {dir}"))
        };
        self.execute(command, message, context).await
    }
    pub async fn remove_remote_path(&self, path: &str, context: &VendorContext) -> VendorResult<()> {
        self.execute(
            vec!["rm".into(), "-rf".into(), path.into()],
            format!("Unable to remove path {path}"),
            context,
        )
        .await
    }
    async fn safe_remove(&self, dst: &str, context: &VendorContext) {
        let _ = self
            .commands
            .execute_worker_command(vec!["rm".into(), "-R".into(), dst.into()], context)
            .await;
    }
    pub async fn copy(
        &self,
        src: &str,
        dst: &str,
        override_existing: bool,
        context: &VendorContext,
    ) -> VendorResult<()> {
        if override_existing {
            self.safe_remove(dst, context).await;
        }
        self.execute(
            vec!["cp".into(), "-R".into(), src.into(), dst.into()],
            format!("failed to copy remote directory {src} to {dst}"),
            context,
        )
        .await
    }
    pub async fn move_file(&self, src: &str, dst: &str, context: &VendorContext) -> VendorResult<()> {
        self.execute(
            vec!["mv".into(), "-f".into(), src.into(), dst.into()],
            format!("failed to move remote file {src} to {dst}"),
            context,
        )
        .await
    }
    pub async fn symlink(
        &self,
        src: &str,
        dst: &str,
        override_existing: bool,
        context: &VendorContext,
    ) -> VendorResult<()> {
        if override_existing {
            self.safe_remove(dst, context).await;
        }
        self.execute(
            vec!["ln".into(), "-s".into(), src.into(), dst.into()],
            format!("failed to symlink remote directory {src} to {dst}"),
            context,
        )
        .await
    }
    pub async fn exists(&self, path: &str, context: &VendorContext) -> VendorResult<Option<bool>> {
        Ok(self
            .commands
            .execute_worker_command(vec!["test".into(), "-e".into(), path.into()], context)
            .await?
            .and_then(|r| match r.exit_code {
                Some(0) => Some(true),
                Some(1) => Some(false),
                _ => None,
            }))
    }
    pub async fn test(&self, path: &str, context: &VendorContext) -> VendorResult<bool> {
        Ok(self
            .commands
            .execute_worker_command(vec!["test".into(), "-f".into(), path.into()], context)
            .await?
            .is_some_and(|r| r.successful()))
    }
    async fn execute(&self, command: Vec<String>, error_message: String, context: &VendorContext) -> VendorResult<()> {
        match self.commands.execute_worker_command(command.clone(), context).await {
            Ok(Some(result)) => {
                let stderr = result.combined_stderr();
                let stderr = kt::trim(&stderr);
                if !kt::is_blank(stderr) && self.logging.enabled(&self.logger, Level::ERROR) {
                    (self.logging.sink)(
                        &self.logger,
                        Level::ERROR,
                        format!("cmd={}, stderr={stderr}", command.join(" ")),
                        None,
                    );
                }
            }
            Ok(None) => {}
            // Kotlin catches Exception, including cancellation, and ignores exit status (A43–A44).
            Err(error) => {
                if self.logging.enabled(&self.logger, Level::ERROR) {
                    (self.logging.sink)(&self.logger, Level::ERROR, error_message, Some(error.failure().clone()));
                }
            }
        }
        Ok(())
    }
}
fn resolve(base: &str, file: &str) -> String {
    if file.starts_with('/') {
        file.into()
    } else {
        format!(
            "{}/{}",
            base.strip_suffix('/').unwrap_or(base),
            file.strip_suffix('/').unwrap_or(file)
        )
    }
}
