//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/extensions/ReaderTest.kt
mod support;
use marathon_apple::{
    cmd::{SessionScope, cancelled, channel::LineChannel, failure},
    extensions::reader::produce_lines_manually,
};
use marathon_core::{error::VendorError, vendor::VendorContext};
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use support::*;
use tokio::io::{AsyncRead, ReadBuf};
#[derive(Clone, Default)]
struct Stream(Arc<Mutex<VecDeque<u8>>>);
impl Stream {
    fn write(&self, value: &str) {
        self.0.lock().unwrap().extend(value.as_bytes());
    }
}
impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut pending = self.0.lock().unwrap();
        if pending.is_empty() {
            return Poll::Pending;
        }
        while buf.remaining() > 0 {
            if let Some(byte) = pending.pop_front() {
                buf.put_slice(&[byte]);
            } else {
                break;
            }
        }
        Poll::Ready(Ok(()))
    }
}
struct Reader {
    stream: Stream,
    output: LineChannel,
    open: Arc<AtomicBool>,
    polls: Arc<AtomicUsize>,
    scope: Arc<SessionScope>,
    task: tokio::task::JoinHandle<marathon_core::error::VendorResult<()>>,
}
impl Reader {
    fn new(context: VendorContext, idle: i64, last: Arc<AtomicI64>) -> Self {
        let stream = Stream::default();
        let input = stream.clone();
        let output = LineChannel::new(64);
        let sink = output.clone();
        let open = Arc::new(AtomicBool::new(true));
        let live = open.clone();
        let polls = Arc::new(AtomicUsize::new(0));
        let count = polls.clone();
        let scope = Arc::new(SessionScope::default());
        let ctx = VendorContext {
            cancellation: scope.clone(),
            ..context
        };
        let task = tokio::spawn(async move {
            produce_lines_manually(input, sink, last, duration(idle), ctx, move || {
                count.fetch_add(1, Ordering::SeqCst);
                live.load(Ordering::SeqCst)
            })
            .await
        });
        Self {
            stream,
            output,
            open,
            polls,
            scope,
            task,
        }
    }
    async fn stop(self) {
        self.scope.cancel(cancelled("test done"));
        self.task.await.unwrap().unwrap();
    }
}
// Tokio's advance jumps to the target; Kotlin advanceTimeBy visits intermediate
// timer deadlines. Visit each millisecond to preserve the Kotlin poll assertions.
async fn advance(millis: u64) {
    for _ in 0..millis {
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn idle_reader_backs_off_to_the_maximum_poll_delay() {
    let reader = Reader::new(context(), 3_600_000, Arc::new(AtomicI64::new(0)));
    tokio::task::yield_now().await;
    advance(10_000).await;
    assert!((500..=510).contains(&reader.polls.load(Ordering::SeqCst)));
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_after_an_idle_period_is_delivered_in_order_and_restarts_the_back_off() {
    let reader = Reader::new(context(), 3_600_000, Arc::new(AtomicI64::new(0)));
    reader.stream.write("first\nsec");
    advance(5_000).await;
    assert_eq!(reader.output.receive().await.unwrap(), Some("first".into()));
    reader.stream.write("ond\nthi");
    let before = reader.polls.load(Ordering::SeqCst);
    advance(40).await;
    assert!(reader.polls.load(Ordering::SeqCst) - before >= 5);
    assert_eq!(reader.output.receive().await.unwrap(), Some("second".into()));
    reader.stream.write("rd");
    advance(5_000).await;
    reader.open.store(false, Ordering::SeqCst);
    advance(100).await;
    assert_eq!(collect(&reader.output).await.unwrap(), ["third"]);
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_read_after_the_idle_deadline_passed_is_delivered_instead_of_an_idle_timeout() {
    let reader = Reader::new(context(), 1000, Arc::new(AtomicI64::new(-60_000)));
    reader.stream.write("late line\n");
    reader.open.store(false, Ordering::SeqCst);
    assert_eq!(collect(&reader.output).await.unwrap(), ["late line"]);
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_of_the_other_stream_that_arrived_before_the_idle_deadline_is_not_reported_as_idle() {
    let ctx = context();
    let last = Arc::new(AtomicI64::new(0));
    let out = Reader::new(ctx.clone(), 1000, last.clone());
    tokio::task::yield_now().await;
    advance(1).await;
    let err = Reader::new(ctx, 1000, last);
    tokio::task::yield_now().await;
    advance(994).await;
    err.stream.write("late\n");
    advance(100).await;
    assert_eq!(err.output.receive().await.unwrap(), Some("late".into()));
    assert!(!out.output.is_closed());
    out.stop().await;
    err.stop().await;
}
#[tokio::test(start_paused = true)]
async fn largest_idle_timeout_never_fires() {
    let reader = Reader::new(context(), i64::MAX, Arc::new(AtomicI64::new(0)));
    tokio::task::yield_now().await;
    advance(10_000).await;
    assert!(!reader.output.is_closed());
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn idle_output_is_closed_with_an_idle_timeout() {
    let clock = Arc::new(Clock::default());
    let mut ctx = context();
    ctx.clock = clock.clone();
    ctx.hooks = clock.clone();
    let reader = Reader::new(ctx, 200, Arc::new(AtomicI64::new(0)));
    assert_eq!(
        collect(&reader.output).await,
        Err(VendorError::Other(failure(
            "java.util.concurrent.TimeoutException",
            "idle timeout PT0.2S reached"
        )))
    );
    assert!((221..=240).contains(&clock.now()), "{}", clock.now());
    reader.stop().await;
}
/// D32: unbuffered xcodebuild output comes in small writes. Each one holds off the idle limit, a line split over them is
/// delivered whole, and the limit still fires once the writes stop.
#[tokio::test(start_paused = true)]
async fn small_writes_of_unbuffered_output_hold_off_the_idle_limit_and_keep_lines_whole_d32() {
    let clock = Arc::new(Clock::default());
    let mut ctx = context();
    ctx.clock = clock.clone();
    ctx.hooks = clock.clone();
    let reader = Reader::new(ctx, 200, Arc::new(AtomicI64::new(0)));
    let line = "Test Case '-[Tests testA]' passed (0.001 seconds).\n";
    // Well past the limit in all, and well inside it between writes.
    for piece in line.as_bytes().chunks(5) {
        advance(100).await;
        reader.stream.write(std::str::from_utf8(piece).unwrap());
    }
    assert!(clock.now() > 200 * 4, "{}", clock.now());
    assert_eq!(
        reader.output.receive().await,
        Ok(Some("Test Case '-[Tests testA]' passed (0.001 seconds).".to_owned()))
    );
    let written = clock.now();
    assert_eq!(
        collect(&reader.output).await,
        Err(VendorError::Other(failure(
            "java.util.concurrent.TimeoutException",
            "idle timeout PT0.2S reached"
        )))
    );
    assert!(clock.now() - written >= 200, "{} after {written}", clock.now());
    reader.stop().await;
}
// Review witness: close-before-drain loses the partial suffix (A24).
#[tokio::test(start_paused = true)]
async fn idle_timeout_discards_tail_and_await_retains_cancellation_control_flow() {
    let ctx = context();
    let reader = Reader::new(ctx.clone(), 200, Arc::new(AtomicI64::new(0)));
    reader.stream.write("partial");
    assert_eq!(
        reader.output.receive().await,
        Err(VendorError::Other(failure(
            "java.util.concurrent.TimeoutException",
            "idle timeout PT0.2S reached",
        )))
    );
    reader.stop().await;
    let base = fake();
    base.stdout.close(Some(failure(
        "java.util.concurrent.TimeoutException",
        "idle timeout PT0.2S reached",
    )));
    match base.await_result(&ctx).await {
        Err(VendorError::Cancelled(marathon_core::error::Cancellation::Requested(detail))) => {
            assert_eq!(detail.message, Some("Channel closed".into()));
            assert_eq!(
                detail.cause.unwrap().class,
                "java.util.concurrent.TimeoutException".into()
            );
        }
        other => panic!("{other:?}"),
    }
}

// A25: EOF with a live process remains idle polling, rather than closing normally.
#[tokio::test(start_paused = true)]
async fn pipe_eof_keeps_polling_until_idle_timeout_while_can_read_is_true() {
    let clock = Arc::new(Clock::default());
    let ctx = VendorContext {
        clock: clock.clone(),
        hooks: clock.clone(),
        ..context()
    };
    let output = LineChannel::new(64);
    produce_lines_manually(
        tokio::io::empty(),
        output.clone(),
        Arc::new(AtomicI64::new(0)),
        duration(20),
        ctx,
        || true,
    )
    .await
    .unwrap();
    assert_eq!(
        output.receive().await,
        Err(VendorError::Other(failure(
            "java.util.concurrent.TimeoutException",
            "idle timeout PT0.02S reached",
        )))
    );
    assert!((41..=60).contains(&clock.now()), "{}", clock.now());
}

#[tokio::test]
async fn cancel_discards_queued_lines_but_keeps_the_first_close_cause() {
    // 4.1 nit carried to WP 4.2: kotlinx channels keep the first close cause.
    use marathon_apple::cmd::{cancelled, channel::LineChannel, failure};
    use marathon_core::error::VendorError;
    let closed = LineChannel::new(4);
    closed.send("queued".into()).await.unwrap();
    closed.close(Some(failure("java.io.IOException", "first")));
    closed.cancel(cancelled("later"));
    assert_eq!(
        closed.receive().await,
        Err(VendorError::Other(failure("java.io.IOException", "first")))
    );
    let cancelled_twice = LineChannel::new(4);
    cancelled_twice.cancel(cancelled("first"));
    cancelled_twice.cancel(cancelled("second"));
    assert_eq!(
        cancelled_twice.receive().await,
        Err(VendorError::Cancelled(cancelled("first")))
    );
}

// D19 (U145): output and EOF that arrive after a poll found nothing and before can_read sees the EOF are still read.
// Kotlin's Reader.kt checks available() and then canRead() → isEOF in the same order and drops them
// (tools/oracle/reader-race-probe).
#[tokio::test(start_paused = true)]
async fn output_arriving_with_eof_between_an_empty_read_and_can_read_is_still_delivered() {
    let stream = Stream::default();
    let arriving = stream.clone();
    let output = LineChannel::new(64);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    produce_lines_manually(
        stream.clone(),
        output.clone(),
        Arc::new(AtomicI64::new(0)),
        duration(60_000),
        context(),
        move || {
            // The SSH pump delivers the command's stdout and then EOF exactly here.
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                arriving.write("HOME=/Users/worker\nUSER=worker\n");
            }
            false
        },
    )
    .await
    .unwrap();
    assert_eq!(collect(&output).await.unwrap(), ["HOME=/Users/worker", "USER=worker"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(stream.0.lock().unwrap().is_empty());
}
