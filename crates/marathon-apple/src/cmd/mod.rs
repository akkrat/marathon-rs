//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandExecutor.kt

pub mod base_command;
pub mod channel;
pub mod command_session_output;
pub mod line_buffer;
pub mod local;
pub mod remote;
pub mod retained_tasks;
pub mod shared_transfers;
pub(crate) mod stall_record;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod write_file;

use crate::extensions::Durations;
pub use base_command::{BaseCommand, CommandSession, ExitCode, SessionControl, SessionScope};
pub use command_session_output::consume_output;
use jcompat::{jtime::Duration, logging::Logger};
use marathon_core::{
    error::{Cancellation, Failure, VendorError, VendorResult},
    runtime::VendorFuture,
    vendor::VendorContext,
};
use remote::ssh::sshj::error::{CONNECTION_EXCEPTION, TRANSPORT_EXCEPTION, is_instance};
use std::{collections::BTreeMap, path::Path};

pub fn failure(class: &str, message: impl AsRef<str>) -> Failure {
    Failure {
        class: class.into(),
        message: Some(message.as_ref().into()),
        cause: None,
        stacktrace: None,
    }
}
pub fn cancelled(message: &str) -> Cancellation {
    Cancellation::Requested(failure("java.util.concurrent.CancellationException", message))
}
pub(crate) fn io_error(error: std::io::Error) -> VendorError {
    VendorError::Other(failure("java.io.IOException", error.to_string()))
}
pub(crate) fn millis(duration: Duration) -> VendorResult<i64> {
    duration
        .to_millis()
        .ok_or_else(|| VendorError::Other(failure("java.lang.ArithmeticException", "long overflow")))
}

/// Port of cmd/CommandHost.kt; charset remains an API slot (UTF-8, approved P1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandHost {
    pub id: String,
    pub charset: String,
}

/// Port of cmd/CommandResult.kt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub exit_code: Option<i32>,
}
impl CommandResult {
    pub fn successful(&self) -> bool {
        self.exit_code == Some(0)
    }
    pub fn combined_stdout(&self) -> String {
        self.stdout.join("\n")
    }
    pub fn combined_stderr(&self) -> String {
        self.stderr.join("\n")
    }
    pub fn successful_or_null(self) -> Option<Self> {
        self.successful().then_some(self)
    }
}

/// Port of cmd/CommandException.kt.
pub struct CommandException;
impl CommandException {
    pub fn for_result(command: &[String], result: &CommandResult) -> Failure {
        // trimMargin() removes no indentation here: the Kotlin string has no '|'.
        failure(
            "com.malinskiy.marathon.apple.cmd.CommandException",
            format!(
                "command {} failed with exit code {}:\n                   stdout: {}\n                   stderr: {}",
                command.join(" "),
                result.exit_code.map_or_else(|| "null".into(), |c| c.to_string()),
                result.combined_stdout(),
                result.combined_stderr()
            ),
        )
    }
}

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub command: Vec<String>,
    pub timeout: Duration,
    pub idle_timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub workdir: Option<String>,
    pub charset: String,
}
impl CommandRequest {
    pub fn new(command: impl IntoIterator<Item = impl Into<String>>, timeout: Duration) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            timeout,
            idle_timeout: Durations::INFINITE,
            env: BTreeMap::new(),
            workdir: None,
            charset: "UTF-8".into(),
        }
    }
}

// Static dispatch within Apple; concrete sessions also suit the future SSH backend.
pub trait CommandExecutor: Send + Sync {
    fn host(&self) -> &CommandHost;
    fn logger(&self) -> &Logger;
    fn connected(&self) -> bool;
    fn log_error(&self, message: String);
    /// `logger.error(e) { message }`; executors whose sink keeps the throwable override this.
    fn log_error_with(&self, message: String, cause: &Failure) {
        let _ = cause;
        self.log_error(message);
    }
    fn execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandSession>;
    fn close(&self) -> VendorFuture<'_, ()>;
    fn critical_execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, CommandResult> {
        Box::pin(async move {
            let session = self.execute(request.clone(), context.clone()).await?;
            let result = session.await_result(&context).await?;
            if result.successful() {
                Ok(result)
            } else {
                Err(VendorError::Other(CommandException::for_result(
                    &request.command,
                    &result,
                )))
            }
        })
    }
    fn safe_execute(&self, request: CommandRequest, context: VendorContext) -> VendorFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            let result = async {
                let session = self.execute(request.clone(), context.clone()).await?;
                session.await_result(&context).await
            }
            .await;
            // Kotlin catches the two sshj classes and their subclasses (OpenFailException is a
            // ConnectionException), in this order.
            let class = |f: &Failure| f.class.to_string_lossy();
            match result {
                Ok(value) => Ok(Some(value)),
                Err(VendorError::Other(ref f)) if is_instance(&class(f), CONNECTION_EXCEPTION) => {
                    self.log_error_with(
                        format!(
                            "Unable to start a remote shell session [{}]",
                            request.command.join(", ")
                        ),
                        f,
                    );
                    Ok(None)
                }
                Err(VendorError::Other(ref f)) if is_instance(&class(f), TRANSPORT_EXCEPTION) => {
                    self.log_error_with(
                        format!("Error starting a remote shell session [{}]", request.command.join(", ")),
                        f,
                    );
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })
    }
    fn safe_execute_nohup(
        &self,
        pidfile: &str,
        mut request: CommandRequest,
        context: VendorContext,
    ) -> VendorFuture<'_, Option<CommandResult>> {
        request.command = vec![
            "sh".into(),
            "-c".into(),
            format!(
                "nohup {} > /dev/null 2>&1 & echo $! > {pidfile}",
                request.command.join(" ")
            ),
        ];
        self.safe_execute(request, context)
    }
}

/// Port of cmd/FileBridge.kt.
pub trait FileBridge: Send + Sync {
    fn send<'a>(&'a self, src: &'a Path, dst: &'a str) -> VendorFuture<'a, bool>;
    fn receive<'a>(&'a self, src: &'a str, dst: &'a Path) -> VendorFuture<'a, bool>;
}

pub(crate) async fn blocking<T: Send + 'static>(
    runtime: &dyn marathon_core::runtime::VendorRuntime,
    body: impl FnOnce() -> VendorResult<T> + Send + 'static,
) -> VendorResult<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = runtime.spawn_blocking(Box::new(move || {
        let _ = tx.send(body());
        Ok(())
    }));
    task.join().await?;
    rx.await
        .map_err(|e| VendorError::Other(failure("java.lang.IllegalStateException", e.to_string())))?
}
