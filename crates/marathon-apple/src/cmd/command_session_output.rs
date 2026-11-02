//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandSessionOutput.kt

use super::{base_command::BaseCommand, cancelled, channel::LineChannel};
use marathon_core::runtime::CancellationSignal;
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use std::future::Future;

struct CancelChannels<'a>(&'a BaseCommand);
impl Drop for CancelChannels<'_> {
    fn drop(&mut self) {
        // Receiver cancellation is the Kotlin finally, not process/session cleanup.
        self.0.stdout.cancel(cancelled("Channel consumption finished"));
        self.0.stderr.cancel(cancelled("Channel consumption finished"));
    }
}
pub async fn consume_output<O, E, OF, EF>(
    session: &BaseCommand,
    context: &VendorContext,
    on_stdout: O,
    on_stderr: E,
) -> VendorResult<Option<i32>>
where
    O: FnMut(String) -> OF,
    E: FnMut(String) -> EF,
    OF: Future<Output = VendorResult<()>>,
    EF: Future<Output = VendorResult<()>>,
{
    let _finally = CancelChannels(session);
    async fn consume<C, F>(channel: &LineChannel, mut callback: C) -> VendorResult<()>
    where
        C: FnMut(String) -> F,
        F: Future<Output = VendorResult<()>>,
    {
        while let Some(line) = channel.receive().await? {
            callback(line).await?;
        }
        Ok(())
    }
    tokio::select! {
        biased;
        reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
        reason = session.scope.cancelled() => Err(VendorError::Cancelled(reason)),
        result = async {
            tokio::try_join!(consume(&session.stdout, on_stdout), consume(&session.stderr, on_stderr))?;
            session.exit_code.wait().await
        } => result,
    }
}
