//! D81: code coverage without `run-as`. No Kotlin source: the Kotlin marathon writes coverage to the app's private data
//! and copies it out with `run-as`, which only a debuggable app allows (C89).
//!
//! The instrumentation runs in the app's process, so the file must go where the app can write and the shell can read
//! without `run-as`: the app's own directory in shared storage. The runner opens the file without creating its
//! directory, so the shell creates it first. A directory the shell creates there belongs to the shell, mode 2770 with
//! group `ext_data_rw`; `o+rwx` lets the app in and the set-group-ID bit gives the app's file that group, which the
//! shell has. On storage that derives permissions instead (API 29 and older) the `chmod` changes nothing and is not
//! needed.
use super::*;
use crate::listeners::{AndroidTestRunListener, to_test};
use jcompat::regex_patterns::PARAMETERIZED_METHOD;
use marathon_adb::testrunner::TestEvent;
use marathon_core::{runtime::*, vendor::VendorContext};
use marathon_logging::Level;

const LOGGER: &str = "marathon_android::coverage";

/// One instrumentation's coverage file: where the runner writes it and what it is called once pulled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageFile {
    /// `<external storage>/Android/data/<application package>`.
    pub app_directory: String,
    pub batch_id: String,
    /// `coverage-<batch id>.ec`; a batch's later instrumentations (D28) add `-2`, `-3` and so on, so that two of them
    /// for the same application package do not write one file.
    pub name: String,
}
impl CoverageFile {
    pub fn new(external_storage: &str, application_package: &str, batch_id: &str, index: usize) -> Self {
        let base = external_storage.strip_suffix('/').unwrap_or(external_storage);
        let name = if index == 0 {
            format!("coverage-{batch_id}.ec")
        } else {
            format!("coverage-{batch_id}-{}.ec", index + 1)
        };
        Self {
            app_directory: format!("{base}/Android/data/{application_package}"),
            batch_id: batch_id.to_owned(),
            name,
        }
    }
    pub fn directory(&self) -> String {
        format!("{}/files/coverage", self.app_directory)
    }
    /// The `coverageFile` instrumentation argument.
    pub fn remote(&self) -> String {
        format!("{}/{}", self.directory(), self.name)
    }
    /// Removes what an earlier run left and creates the directory the app can write to. Its exit code is `mkdir`'s:
    /// a `chmod` fails on a directory the app already owns and on storage that derives permissions, and neither matters.
    pub fn prepare_command(&self) -> String {
        let app = &self.app_directory;
        let directory = self.directory();
        format!(
            "rm -rf {directory}; mkdir -p {directory} && {{ chmod o+rwx {app} {app}/files {directory} 2>/dev/null; true; }}"
        )
    }
    pub fn remove_command(&self) -> String {
        format!("rm -rf {}", self.directory())
    }
}

/// Prepares the coverage directory, after `pm clear` and the listeners' file-sync setup, either of which would remove it. A failure is reported, not raised: the
/// tests can still run, and the missing file is reported again when it is pulled.
pub(crate) async fn prepare(device: &AndroidDevice, file: &CoverageFile, context: VendorContext) -> VendorResult<()> {
    let directory = file.directory();
    let message = format!(
        "Unable to create the coverage directory {directory} of batch {}",
        file.batch_id
    );
    let result = device
        .safe_execute_shell_command(&file.prepare_command(), &message, context)
        .await?;
    if let Some(result) = result
        && result.exit_code != 0
    {
        device.log_named(
            LOGGER,
            Level::ERROR,
            &format!("{message}: exit code {}, {}", result.exit_code, trim(&result.output)),
            None,
        );
    }
    Ok(())
}

/// Pulls each instrumentation's coverage file into `device-files/<pool>/coverage/`, the folder the Kotlin file-sync entry used, and attaches it to every test
/// the instrumentation started, so each test's result names the file of its batch.
pub struct CoverageTestRunListener {
    pub artifacts: ArtifactContext,
    /// Tests started since the last pull, in order, each once.
    started: Mutex<Vec<Test>>,
    attachments: Attachments,
}
impl CoverageTestRunListener {
    pub fn new(artifacts: ArtifactContext) -> Self {
        Self {
            artifacts,
            started: Mutex::default(),
            attachments: Attachments::default(),
        }
    }
    fn error(&self, message: &str, cause: Option<&Failure>) {
        self.artifacts.device.log_named(LOGGER, Level::ERROR, message, cause);
    }
    async fn local(&self, name: &str) -> VendorResult<PathBuf> {
        let device = &self.artifacts.device;
        let folder = device
            .runtime()
            .dependencies
            .files
            .create_folder(FolderType::DeviceFiles, Some(&self.artifacts.pool), None)
            .await
            .map_err(operation_error)?
            .join("coverage");
        let file = folder.join(name);
        blocking(device.runtime().dependencies.io.as_ref(), move || {
            std::fs::create_dir_all(&folder).map_err(io)?;
            let cwd = std::env::current_dir().map_err(io)?;
            Ok(cwd.join(file))
        })
        .await
    }
    /// The pulled file, or why there is none. Cancellation is the only error.
    async fn pull(&self, file: &CoverageFile, context: &VendorContext) -> VendorResult<Result<PathBuf, String>> {
        let device = &self.artifacts.device;
        let local = match self.local(&file.name).await {
            Ok(local) => local,
            Err(VendorError::Cancelled(c)) => return Err(VendorError::Cancelled(c)),
            Err(e) => return Ok(Err(e.failure().to_string())),
        };
        let remote = file.remote();
        let target = local.to_string_lossy().into_owned();
        let timeout = millis(device.runtime().configuration.timeout_configuration.pull_file)?;
        let pulled = device.receive_with_timeout(&remote, &target, context.clone()).await;
        match pulled {
            Ok(Some(())) => {}
            Ok(None) => return Ok(Err(format!("pulling it timed out after {timeout} ms"))),
            Err(VendorError::Cancelled(c)) => return Err(VendorError::Cancelled(c)),
            Err(e) => return Ok(Err(e.failure().to_string())),
        }
        let path = local.clone();
        let size = blocking(device.runtime().dependencies.io.as_ref(), move || {
            Ok(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0))
        })
        .await?;
        if size == 0 {
            return Ok(Err("the file is empty".into()));
        }
        Ok(Ok(local))
    }
}
impl AttachmentProvider for CoverageTestRunListener {
    fn register_listener(&self, l: Arc<dyn AttachmentListener>) -> VendorResult<()> {
        self.attachments.register(l)
    }
}
impl AndroidTestRunListener for CoverageTestRunListener {
    fn on_event_in<'a>(&'a self, event: &'a TestEvent, _context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            if let TestEvent::TestStarted { id } = event {
                let test = to_test(id);
                let mut started = lock(&self.started);
                if !started.contains(&test) {
                    started.push(test);
                }
            }
            Ok(())
        })
    }
    fn on_coverage_in<'a>(&'a self, file: &'a CoverageFile, context: VendorContext) -> VendorFuture<'a, ()> {
        Box::pin(async move {
            let tests = std::mem::take(&mut *lock(&self.started));
            if context.cancellation.reason().is_some() {
                return Ok(());
            }
            let remote = file.remote();
            match self.pull(file, &context).await {
                Ok(Ok(local)) => {
                    // TestResultsListener merges a parameterised test's runs into one result for the method without
                    // its index, and looks attachments up by that name, so the merged name gets the file as well.
                    let mut keys: Vec<Test> = vec![];
                    for test in tests {
                        let merged = PARAMETERIZED_METHOD.matches(&test.method).then(|| {
                            let mut merged = test.clone();
                            merged
                                .method
                                .truncate(merged.method.find('[').unwrap_or(merged.method.len()));
                            merged.meta_properties.clear();
                            merged
                        });
                        for key in std::iter::once(test).chain(merged) {
                            if !keys.contains(&key) {
                                keys.push(key);
                            }
                        }
                    }
                    for key in &keys {
                        self.attachments
                            .emit(key, local.clone(), AttachmentType::Coverage, "coverage")?;
                    }
                }
                Ok(Err(reason)) => {
                    self.error(
                        &format!(
                            "Code coverage of batch {} is missing: {remote} was not written or could not be pulled ({reason})",
                            file.batch_id
                        ),
                        None,
                    );
                    return Ok(());
                }
                Err(e) => {
                    self.error(
                        &format!("Code coverage of batch {} was not delivered: {remote}", file.batch_id),
                        Some(e.failure()),
                    );
                    return Err(e);
                }
            }
            self.artifacts
                .device
                .safe_execute_shell_command(
                    &file.remove_command(),
                    &format!("Unable to remove the coverage directory {}", file.directory()),
                    context,
                )
                .await?;
            Ok(())
        })
    }
}
