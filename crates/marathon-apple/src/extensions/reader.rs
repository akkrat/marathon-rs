//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/extensions/Reader.kt

use crate::cmd::{channel::LineChannel, failure, line_buffer::LineBuffer, millis};
use jcompat::jtime::Duration;
use marathon_core::{
    error::{VendorError, VendorResult},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::{future::Future, io};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
};

/// The last positive read on either stream, in the command's vendor-clock domain.
#[derive(Clone)]
pub struct OutputActivity(watch::Sender<i64>);
impl OutputActivity {
    pub fn new(now: i64) -> Self {
        Self(watch::channel(now).0)
    }
    fn record(&self, now: i64) {
        // Notify even when two reads have the same millisecond timestamp.
        self.0.send_replace(now);
    }
}

/// Running reads register readiness; terminal drains and idle-boundary probes
/// must also see bytes buffered before the reactor delivers readiness (D19).
pub trait ReaderInput: AsyncRead + Unpin {
    fn read_buffered(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>>;
}

fn remaining(last: i64, now: i64, limit: i64) -> i64 {
    if limit <= 0 {
        return 0;
    }
    (i128::from(last) + i128::from(limit) - i128::from(now)).clamp(0, i128::from(i64::MAX)) as i64
}

/// D62: readiness, a shared vendor-clock idle deadline and a latched backend end
/// replace polling. Ready bytes retain precedence over idle expiry and end.
pub async fn produce_lines<R, E>(
    mut input: R,
    output: LineChannel,
    activity: OutputActivity,
    idle_timeout: Duration,
    context: VendorContext,
    ended: E,
) -> VendorResult<()>
where
    R: ReaderInput,
    E: Future<Output = ()>,
{
    let sink = output.clone();
    let buffer = LineBuffer::new(move |line| {
        let sink = sink.clone();
        Box::pin(async move { sink.send(line).await })
    });
    let result = async {
        let limit = millis(idle_timeout);
        let mut last = activity.0.subscribe();
        let mut bytes = [0; 16_384];
        let mut eof = false;
        let mut chunks = 0_u32;
        tokio::pin!(ended);
        loop {
            if let Some(reason) = context.cancellation.reason() {
                return Err(VendorError::Cancelled(reason));
            }
            if output.is_closed() {
                break;
            }
            let timestamp = *last.borrow_and_update();
            let wait = limit
                .as_ref()
                .map_or(0, |limit| remaining(timestamp, context.clock.now_millis(), *limit));
            tokio::select! {
                biased;
                read = input.read(&mut bytes), if !eof => {
                    match read {
                        Ok(0) => eof = true, // A25: keep output open without repeatedly reading EOF.
                        Ok(count) => {
                            buffer.append(&bytes[..count]);
                            activity.record(context.clock.now_millis());
                            buffer.flush().await?;
                            chunks += 1;
                        }
                        Err(_) => break,
                    }
                }
                _ = last.changed() => {}
                result = async {
                    // Keep conversion failure behind a ready read/error, as in Reader.kt.
                    match &limit {
                        Ok(_) => context.clock.delay(wait, context.cancellation.clone()).await,
                        Err(error) => Err(error.clone()),
                    }
                }, if !matches!(limit, Ok(i64::MAX)) => {
                    // Reactor readiness can lag behind bytes in a local pipe. A single
                    // boundary probe keeps readable bytes ahead of timeout, too (D19).
                    if !eof {
                        match input.read_buffered(&mut bytes) {
                            Ok(Some(count)) if count > 0 => {
                                buffer.append(&bytes[..count]);
                                activity.record(context.clock.now_millis());
                                buffer.flush().await?;
                                chunks += 1;
                                if chunks >= 32 { tokio::task::yield_now().await; chunks = 0; }
                                continue;
                            }
                            Err(_) => break,
                            _ => {}
                        }
                    }
                    result?;
                    let limit = *limit.as_ref().map_err(Clone::clone)?;
                    // The sibling may have read while this deadline was becoming ready.
                    if remaining(*last.borrow(), context.clock.now_millis(), limit) == 0 {
                        let cause = failure(FailureKind::Timeout, format!("idle timeout {idle_timeout} reached"));
                        output.close(Some(cause.clone()));
                        return Err(VendorError::Other(cause));
                    }
                }
                _ = &mut ended => {
                    // D19/U145: never wait for inherited write ends, or trust stale
                    // reactor readiness. These reads neither refresh activity nor check idle.
                    loop {
                        if let Some(reason) = context.cancellation.reason() { return Err(VendorError::Cancelled(reason)); }
                        if output.is_closed() { break; }
                        let Ok(Some(count)) = input.read_buffered(&mut bytes) else { break; };
                        if count == 0 { break; }
                        buffer.append(&bytes[..count]);
                        buffer.flush().await?;
                        chunks += 1;
                        if chunks >= 32 { tokio::task::yield_now().await; chunks = 0; }
                    }
                    break;
                }
            }
            // In-memory SSH buffers and final drains need not yield on their own.
            if chunks >= 32 {
                tokio::task::yield_now().await;
                chunks = 0;
            }
        }
        Ok(())
    };
    let result = tokio::select! {
        biased;
        reason = context.cancellation.cancelled() => Err(VendorError::Cancelled(reason)),
        _ = output.closed() => Ok(()),
        result = result => result,
    };
    // A24: timeout closes before the tail drain; the closed channel rejects it.
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
