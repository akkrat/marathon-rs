//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/Reader.kt

use super::poll_backoff::PollBackoff;
use crate::cmd::{channel::LineChannel, failure, line_buffer::LineBuffer, millis};
use jcompat::jtime::Duration;
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    task::Poll,
};
use tokio::io::{AsyncRead, ReadBuf};

pub fn current_time_millis(context: &VendorContext) -> VendorResult<i64> {
    context
        .hooks
        .millis("apple.Reader.currentTimeMillis")
        .map_err(|e| VendorError::Other(failure("ReplayError", String::from_utf16_lossy(&e.message.0))))
}

/// Polls an async stream once per backoff tick. A pending read never delays the
/// idle check, and available data wins over both idle expiry and can_read=false.
pub async fn produce_lines_manually<R, C>(
    mut input: R,
    output: LineChannel,
    last_output: Arc<AtomicI64>,
    idle_timeout: Duration,
    context: VendorContext,
    can_read: C,
) -> VendorResult<()>
where
    R: AsyncRead + Unpin,
    C: Fn() -> bool,
{
    let sink = output.clone();
    let buffer = LineBuffer::new(move |line| {
        let sink = sink.clone();
        Box::pin(async move { sink.send(line).await })
    });
    let result = async {
        let mut backoff = PollBackoff::default();
        let mut bytes = [0; 16_384];
        loop {
            if context.cancellation.reason().is_some() || output.is_closed() {
                break;
            }
            let count = match read_now(&mut input, &mut bytes).await {
                Ok(count) => count,
                Err(_) => break,
            };
            if let Some(count) = count.filter(|count| *count > 0) {
                buffer.append(&bytes[..count]);
                last_output.store(current_time_millis(&context)?, Ordering::SeqCst);
                backoff.reset();
                buffer.flush().await?;
                continue;
            }
            let elapsed = current_time_millis(&context)?.wrapping_sub(last_output.load(Ordering::SeqCst));
            if elapsed.wrapping_sub(backoff.max_millis) > millis(idle_timeout)? {
                let cause = failure(
                    "java.util.concurrent.TimeoutException",
                    format!("idle timeout {idle_timeout} reached"),
                );
                output.close(Some(cause.clone()));
                return Err(VendorError::Other(cause));
            }
            // Kotlin never reads when available()==0, including pipe EOF.
            // Only can_read ends that polling while the process is alive (A25).
            if !can_read() {
                // D19: output that arrived together with EOF after this poll's empty read is still buffered.
                // Kotlin stops here without reading it (U145); read it before the drain below.
                while context.cancellation.reason().is_none() && !output.is_closed() {
                    match read_now(&mut input, &mut bytes).await {
                        Ok(Some(count)) if count > 0 => {
                            buffer.append(&bytes[..count]);
                            buffer.flush().await?;
                        }
                        _ => break,
                    }
                }
                break;
            }
            context
                .clock
                .delay(backoff.next_millis(), context.cancellation.clone())
                .await?;
        }
        Ok(())
    };
    let result = tokio::select! {
        biased;
        reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
        result = result => result,
    };
    // Kotlin closes with the timeout before LineBuffer.drain tries to send its
    // tail. The closed channel rejects that tail (A24).
    let drain = tokio::select! {
        biased;
        _ = context.cancellation.cancelled() => Ok(()),
        result = buffer.drain() => result,
    };
    match result.and(drain) {
        Ok(()) => {
            output.close(None);
            Ok(())
        }
        Err(VendorError::Other(cause)) => {
            output.close(Some(cause));
            Ok(())
        }
        Err(VendorError::Cancelled(reason)) => {
            output.cancel(reason);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// One non-waiting read: `Some(0)` at EOF, `None` while nothing is available.
async fn read_now<R: AsyncRead + Unpin>(input: &mut R, bytes: &mut [u8]) -> std::io::Result<Option<usize>> {
    std::future::poll_fn(|cx| {
        let mut buf = ReadBuf::new(bytes);
        Poll::Ready(match Pin::new(&mut *input).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(())) => Ok(Some(buf.filled().len())),
            Poll::Ready(Err(error)) => Err(error),
            Poll::Pending => Ok(None),
        })
    })
    .await
}
