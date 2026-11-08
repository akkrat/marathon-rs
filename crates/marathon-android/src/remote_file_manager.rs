//! Port of vendor/vendor-android/src/main/kotlin/com/malinskiy/marathon/android/RemoteFileManager.kt
use crate::{device::AndroidDevice, support::*};
use jcompat::{file_manager::escape_utf16, logging::Level};
use marathon_adb::client::sync::directory::bash_escape;
use marathon_core::{error::VendorResult, model::Test, vendor::VendorContext};
pub const MAX_FILENAME: usize = 255;
pub const TMP_PATH: &str = "/data/local/tmp";
pub const TRACE_ROOT: &str = "/data/misc/perfetto-traces";
pub const TRACE_CONFIG_FILE: &str = "/data/local/tmp/tracing.pbtx";
pub struct RemoteFileManager<'a> {
    pub device: &'a AndroidDevice,
}
/// `path` as one word for the device's shell: as it is if nothing in it is special to the shell, quoted otherwise.
fn shell_word(path: &str) -> std::borrow::Cow<'_, str> {
    let plain = !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+,:@%".contains(&b));
    if plain { path.into() } else { bash_escape(path).into() }
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
    /// U159's removal of a folder push's destination, which Kotlin doesn't send, so its command is not bound to
    /// `remove_remote_path`'s: the path is quoted for the device's shell, as a folder name may hold a space, and the
    /// removal must leave nothing there, or adb's push would land one level inside the leftover. An absent destination
    /// is fine: `rm` fails on it and the test then passes. `-e` follows a link, so a dangling one that `rm` could not
    /// remove needs `-L` as well.
    pub async fn remove_push_destination(&self, path: &str, context: VendorContext) -> VendorResult<()> {
        let quoted = bash_escape(path);
        let message = format!("Could not delete remote file(s): {path}");
        let result = self
            .device
            .critical_execute_shell_command(
                &format!("rm -r -- {quoted} || {{ [ ! -e {quoted} ] && [ ! -L {quoted} ]; }}"),
                &message,
                context,
            )
            .await?;
        if result.exit_code != 0 {
            return Err(other(
                "com.malinskiy.marathon.android.exception.CommandRejectedException",
                format!(
                    "{message}: exit code {}: {}",
                    result.exit_code,
                    result.output.to_owned().trim()
                ),
            ));
        }
        Ok(())
    }
    /// Kotlin's after-batch removal of a pushed path, `rm -r <path>`, with the path quoted only when the device's shell
    /// would split or expand it (N41). An ordinary path gives Kotlin's command byte for byte. The exit code is still not
    /// read, as in Kotlin.
    pub async fn remove_pushed_path(&self, path: &str, context: VendorContext) -> VendorResult<()> {
        self.device
            .critical_execute_shell_command(
                &format!("rm -r {}", shell_word(path)),
                &format!("Could not delete remote file(s): {path}"),
                context,
            )
            .await?;
        Ok(())
    }
    pub async fn create_remote_directory(&self, path: Option<&str>, context: VendorContext) -> VendorResult<()> {
        let output = self.device.external_storage_mount().to_owned();
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
        let path = self.device.external_storage_mount().to_owned();
        self.device
            .critical_execute_shell_command(
                &format!("rm -r {path}"),
                &format!("Could not delete remote directory: {path}"),
                context,
            )
            .await?;
        Ok(())
    }
    pub fn remote_video_for_test(&self, test: &Test, batch: &String) -> VendorResult<String> {
        self.remote_chunked_video_for_test(test, batch, None)
    }
    pub fn remote_chunked_video_for_test(
        &self,
        test: &Test,
        batch: &String,
        chunk: Option<i64>,
    ) -> VendorResult<String> {
        let mut path = self.device.external_storage_mount();
        path.push('/');
        path.push_str(&self.filename(test, batch, "mp4", chunk)?);
        Ok(path)
    }
    pub fn remote_profiling_for_test(&self, test: &Test, batch: &String) -> VendorResult<String> {
        let mut path = String::from(TRACE_ROOT);
        path.push('/');
        path.push_str(&self.filename(test, batch, "perfetto-trace", None)?);
        Ok(path)
    }
    fn filename(&self, test: &Test, batch: &String, extension: &str, chunk: Option<i64>) -> VendorResult<String> {
        let mut suffix = String::from("-");
        suffix.push_str(&batch.clone());
        if let Some(chunk) = chunk {
            suffix.push_str(&format!("-{chunk}"));
        }
        suffix.push('.');
        suffix.push_str(extension);
        let count = MAX_FILENAME.checked_sub(suffix.encode_utf16().count()).ok_or_else(|| {
            other(
                "java.lang.IllegalArgumentException",
                format!(
                    "Requested character count {} is less than zero.",
                    MAX_FILENAME as i64 - suffix.encode_utf16().count() as i64
                )
                .as_str(),
            )
        })?;
        let mut raw = test.to_class_name('.');
        raw.push('-');
        raw.push_str(&test.method.clone());
        let raw = escape_utf16(&raw.encode_utf16().collect::<Vec<_>>());
        let mut result = String::from_utf16_lossy(&raw[..raw.len().min(count)]);
        result.push_str(&suffix);
        if raw.len() > count {
            self.device.log_named("com.malinskiy.marathon.android.RemoteFileManager",Level::Error,&format!("Remote filename length cannot exceed 255 characters and has been trimmed to {} and can create a conflict. This happened because the combination of test class name and test name is too long.",result.to_owned()),None);
        }
        Ok(result)
    }
}
