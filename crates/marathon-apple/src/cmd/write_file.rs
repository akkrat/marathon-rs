//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/WriteFile.kt

use super::{CommandExecutor, CommandRequest, remote::ssh::sshj::config::SshLogging};
use base64::{Engine, engine::general_purpose::STANDARD};
use jcompat::{jtime::Duration, logging::Level};
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};

pub const MAX_WRITTEN_FILE_SIZE: usize = 32 * 1024;

pub async fn write_file(
    executor: &dyn CommandExecutor,
    content: &[u8],
    dst: &str,
    timeout: Duration,
    context: VendorContext,
    logging: &SshLogging,
) -> VendorResult<bool> {
    // This fixed ASCII grammar is the source Regex's full match, including '..'.
    if content.len() > MAX_WRITTEN_FILE_SIZE
        || dst.len() < 2
        || !dst.starts_with('/')
        || !dst
            .bytes()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || b"_./@+-".contains(&c))
    {
        return Ok(false);
    }
    let temporary = format!("{dst}.marathon-tmp");
    let encoded = STANDARD.encode(content);
    let script = format!(
        "umask 077 && echo {encoded} | base64 --decode > {temporary} && [ $(wc -c < {temporary}) -eq {} ] && mv -f {temporary} {dst} || {{ rm -f {temporary}; exit 1; }}",
        content.len()
    );
    let result = async {
        let session = executor
            .execute(
                CommandRequest::new(["sh".to_owned(), "-c".to_owned(), script], timeout),
                context.clone(),
            )
            .await?;
        session.await_result(&context).await.map(|result| result.successful())
    }
    .await;
    match result {
        Ok(success) => Ok(success),
        Err(error @ VendorError::Cancelled(_)) => Err(error),
        Err(error) => {
            if logging.enabled(executor.logger(), Level::Debug) {
                let cause = match error {
                    VendorError::Other(failure) => Some(failure),
                    _ => None,
                };
                (logging.sink)(executor.logger(), Level::Debug, format!("Unable to write {dst}"), cause);
            }
            Ok(false)
        }
    }
}
