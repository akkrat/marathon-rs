//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/RemoteFileManager.kt
use crate::{device::AndroidDevice, support::*};
use jcompat::{file_manager::escape_utf16, logging::Level, writers::JavaString};
use marathon_core::{error::VendorResult, model::Test, vendor::VendorContext};
pub const MAX_FILENAME: usize = 255;
pub const TMP_PATH: &str = "/data/local/tmp";
pub const TRACE_ROOT: &str = "/data/misc/perfetto-traces";
pub const TRACE_CONFIG_FILE: &str = "/data/local/tmp/tracing.pbtx";
pub struct RemoteFileManager<'a> {
    pub device: &'a AndroidDevice,
}
impl RemoteFileManager<'_> {
    pub async fn remove_remote_path(&self, path: &str, recursive: bool, context: VendorContext) -> VendorResult<()> {
        self.device
            .critical_execute_shell_command(
                &format!("rm {}{path}", if recursive { "-r " } else { "" }),
                &format!("Could not delete remote file(s): {path}"),
                context,
            )
            .await?;
        Ok(())
    }
    pub async fn create_remote_directory(&self, path: Option<&str>, context: VendorContext) -> VendorResult<()> {
        let output = self.device.external_storage_mount().to_string_lossy();
        let path = path.unwrap_or(&output);
        self.device
            .critical_execute_shell_command(
                &format!("mkdir {path}"),
                &format!("Could not create remote directory: {path}"),
                context,
            )
            .await?;
        Ok(())
    }
    pub async fn remove_remote_directory(&self, context: VendorContext) -> VendorResult<()> {
        let path = self.device.external_storage_mount().to_string_lossy();
        self.device
            .critical_execute_shell_command(
                &format!("rm -r {path}"),
                &format!("Could not delete remote directory: {path}"),
                context,
            )
            .await?;
        Ok(())
    }
    pub fn remote_video_for_test(&self, test: &Test, batch: &JavaString) -> VendorResult<JavaString> {
        self.remote_chunked_video_for_test(test, batch, None)
    }
    pub fn remote_chunked_video_for_test(
        &self,
        test: &Test,
        batch: &JavaString,
        chunk: Option<i64>,
    ) -> VendorResult<JavaString> {
        let mut path = self.device.external_storage_mount();
        path.push_str("/");
        path.append(self.filename(test, batch, "mp4", chunk)?);
        Ok(path)
    }
    pub fn remote_profiling_for_test(&self, test: &Test, batch: &JavaString) -> VendorResult<JavaString> {
        let mut path = JavaString::from(TRACE_ROOT);
        path.push_str("/");
        path.append(self.filename(test, batch, "perfetto-trace", None)?);
        Ok(path)
    }
    fn filename(
        &self,
        test: &Test,
        batch: &JavaString,
        extension: &str,
        chunk: Option<i64>,
    ) -> VendorResult<JavaString> {
        let mut suffix = JavaString::from("-");
        suffix.append(batch.clone());
        if let Some(chunk) = chunk {
            suffix.push_str(&format!("-{chunk}"));
        }
        suffix.push_str(".");
        suffix.push_str(extension);
        let count = MAX_FILENAME.checked_sub(suffix.0.len()).ok_or_else(|| {
            other(
                "java.lang.IllegalArgumentException",
                format!(
                    "Requested character count {} is less than zero.",
                    MAX_FILENAME as i64 - suffix.0.len() as i64
                )
                .as_str(),
            )
        })?;
        let mut raw = test.to_class_name('.');
        raw.push_str("-");
        raw.append(test.method.clone());
        let raw = escape_utf16(&raw.0);
        let mut result = JavaString(raw[..raw.len().min(count)].to_vec());
        result.append(suffix);
        if raw.len() > count {
            self.device.log_named("com.malinskiy.marathon.android.RemoteFileManager",Level::Error,&format!("Remote filename length cannot exceed 255 characters and has been trimmed to {} and can create a conflict. This happened because the combination of test class name and test name is too long.",result.to_string_lossy()),None);
        }
        Ok(result)
    }
}
