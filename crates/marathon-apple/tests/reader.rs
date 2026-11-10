//! Port of vendor/vendor-apple/base/src/test/kotlin/com/malinskiy/marathon/apple/extensions/ReaderTest.kt
use marathon_error::FailureKind;
mod support;
use marathon_apple::{
    cmd::{SessionScope, cancelled, channel::LineChannel, failure},
    extensions::reader::{OutputActivity, ReaderInput, produce_lines},
};
use marathon_core::{error::VendorError, vendor::VendorContext};
use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use support::*;
use tokio::io::{AsyncRead, ReadBuf};
#[derive(Default)]
struct StreamState {
    bytes: VecDeque<u8>,
    waker: Option<Waker>,
    eof: bool,
    polls: usize,
    poll_thread: Option<std::thread::ThreadId>,
}
#[derive(Clone, Default)]
struct Stream(Arc<Mutex<StreamState>>);
impl Stream {
    fn write(&self, value: &str) {
        self.write_bytes(value.as_bytes());
    }
    fn write_bytes(&self, value: &[u8]) {
        let mut state = self.0.lock().unwrap();
        state.bytes.extend(value);
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn eof(&self) {
        let mut state = self.0.lock().unwrap();
        state.eof = true;
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
    fn polls(&self) -> usize {
        self.0.lock().unwrap().polls
    }
}
impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let mut state = self.0.lock().unwrap();
        state.polls += 1;
        state.poll_thread = Some(std::thread::current().id());
        if state.bytes.is_empty() && !state.eof && buf.remaining() > 0 {
            state.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        while buf.remaining() > 0 {
            if let Some(byte) = state.bytes.pop_front() {
                buf.put_slice(&[byte]);
            } else {
                break;
            }
        }
        Poll::Ready(Ok(()))
    }
}
impl ReaderInput for Stream {
    fn read_buffered(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>> {
        let mut state = self.0.lock().unwrap();
        let count = bytes.len().min(state.bytes.len());
        for byte in &mut bytes[..count] {
            *byte = state.bytes.pop_front().unwrap();
        }
        Ok(Some(count))
    }
}
struct Reader {
    stream: Stream,
    output: LineChannel,
    end: tokio::sync::watch::Sender<bool>,
    scope: Arc<SessionScope>,
    task: tokio::task::JoinHandle<marathon_core::error::VendorResult<()>>,
}
impl Reader {
    fn new(context: VendorContext, idle: i64, last: OutputActivity) -> Self {
        Self::with_capacity(context, idle, last, 64)
    }
    fn with_capacity(context: VendorContext, idle: i64, last: OutputActivity, capacity: usize) -> Self {
        let stream = Stream::default();
        let input = stream.clone();
        let output = LineChannel::new(capacity);
        let sink = output.clone();
        let (end, mut ended) = tokio::sync::watch::channel(false);
        let scope = Arc::new(SessionScope::default());
        let ctx = VendorContext {
            cancellation: scope.clone(),
            ..context
        };
        let task = tokio::spawn(async move {
            produce_lines(input, sink, last, duration(idle), ctx, async move {
                while !*ended.borrow_and_update() {
                    ended.changed().await.unwrap();
                }
            })
            .await
        });
        Self {
            stream,
            output,
            end,
            scope,
            task,
        }
    }
    fn end(&self) {
        self.end.send_replace(true);
    }
    async fn stop(self) {
        self.scope.cancel(cancelled("test done"));
        assert_eq!(self.task.await.unwrap(), Ok(()));
    }
}
async fn advance(millis: u64) {
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(millis)).await;
    tokio::task::yield_now().await;
}
#[tokio::test(start_paused = true)]
async fn silent_reader_waits_without_periodic_wakeups_d62() {
    let reader = Reader::new(context(), 3_600_000, OutputActivity::new(0));
    tokio::task::yield_now().await;
    let polls = reader.stream.polls();
    advance(10_000).await;
    assert_eq!(reader.stream.polls(), polls);
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_after_an_idle_period_is_delivered_in_order_and_restarts_the_back_off() {
    let reader = Reader::new(context(), 3_600_000, OutputActivity::new(0));
    reader.stream.write("first\nsec");
    advance(5_000).await;
    assert_eq!(reader.output.receive().await.unwrap(), Some("first".into()));
    reader.stream.write("ond\nthi");
    advance(40).await;
    assert_eq!(reader.output.receive().await.unwrap(), Some("second".into()));
    reader.stream.write("rd");
    advance(5_000).await;
    reader.end();
    advance(100).await;
    assert_eq!(collect(&reader.output).await.unwrap(), ["third"]);
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_read_after_the_idle_deadline_passed_is_delivered_instead_of_an_idle_timeout() {
    let reader = Reader::new(context(), 1000, OutputActivity::new(-60_000));
    reader.stream.write("late line\n");
    reader.end();
    assert_eq!(collect(&reader.output).await.unwrap(), ["late line"]);
    reader.stop().await;
}
#[tokio::test(start_paused = true)]
async fn output_of_the_other_stream_that_arrived_before_the_idle_deadline_is_not_reported_as_idle() {
    let ctx = context();
    let last = OutputActivity::new(0);
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
    let reader = Reader::new(context(), i64::MAX, OutputActivity::new(0));
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
    let reader = Reader::new(ctx, 200, OutputActivity::new(0));
    assert_eq!(
        collect(&reader.output).await,
        Err(VendorError::Other(failure(
            FailureKind::Timeout,
            "idle timeout PT0.2S reached"
        )))
    );
    assert_eq!(clock.now(), 200);
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
    let reader = Reader::new(ctx, 200, OutputActivity::new(0));
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
            FailureKind::Timeout,
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
    let reader = Reader::new(ctx.clone(), 200, OutputActivity::new(0));
    reader.stream.write("partial");
    assert_eq!(
        reader.output.receive().await,
        Err(VendorError::Other(failure(
            FailureKind::Timeout,
            "idle timeout PT0.2S reached",
        )))
    );
    reader.stop().await;
    let base = fake();
    base.stdout
        .close(Some(failure(FailureKind::Timeout, "idle timeout PT0.2S reached")));
    match base.await_result(&ctx).await {
        Err(VendorError::Cancelled(marathon_core::error::Cancellation::Requested(detail))) => {
            assert_eq!(detail.message, Some("Channel closed".into()));
            assert_eq!(detail.cause.unwrap().kind.description(), "timed out".to_owned());
        }
        other => panic!("{other:?}"),
    }
}

// A25: local EOF disables reads but retains the idle deadline and channel lifetime.
#[tokio::test(start_paused = true)]
async fn pipe_eof_waits_until_idle_timeout_while_process_is_alive() {
    let reader = Reader::new(context(), 20, OutputActivity::new(0));
    reader.stream.eof();
    advance(19).await;
    assert!(!reader.output.is_closed());
    let polls = reader.stream.polls();
    advance(1).await;
    assert_eq!(reader.output.receive().await, Err(idle_error(20)));
    assert_eq!(reader.stream.polls(), polls);
    reader.stop().await;
}

#[tokio::test]
async fn cancel_discards_queued_lines_but_keeps_the_first_close_cause() {
    // 4.1 nit carried to WP 4.2: kotlinx channels keep the first close cause.
    use marathon_apple::cmd::{cancelled, channel::LineChannel, failure};
    use marathon_core::error::VendorError;
    let closed = LineChannel::new(4);
    closed.send("queued".into()).await.unwrap();
    closed.close(Some(failure(FailureKind::Io, "first")));
    closed.cancel(cancelled("later"));
    assert_eq!(
        closed.receive().await,
        Err(VendorError::Other(failure(FailureKind::Io, "first")))
    );
    let cancelled_twice = LineChannel::new(4);
    cancelled_twice.cancel(cancelled("first"));
    cancelled_twice.cancel(cancelled("second"));
    assert_eq!(
        cancelled_twice.receive().await,
        Err(VendorError::Cancelled(cancelled("first")))
    );
}

// D19: data and end arrive inside the pending read, before the end future is polled.
#[tokio::test(start_paused = true)]
async fn output_arriving_with_eof_between_an_empty_read_and_can_read_is_still_delivered() {
    struct Race {
        stream: Stream,
        end: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl AsyncRead for Race {
        fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            if let Some(end) = self.end.take() {
                self.stream.write("first\nsecond tail");
                assert!(end.send(()).is_ok());
            }
            Poll::Pending
        }
    }
    impl ReaderInput for Race {
        fn read_buffered(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>> {
            self.stream.read_buffered(bytes)
        }
    }
    let stream = Stream::default();
    let (end, ended) = tokio::sync::oneshot::channel();
    let output = LineChannel::new(64);
    assert_eq!(
        produce_lines(
            Race {
                stream: stream.clone(),
                end: Some(end)
            },
            output.clone(),
            OutputActivity::new(0),
            duration(60_000),
            context(),
            async move {
                assert!(ended.await.is_ok());
            }
        )
        .await,
        Ok(())
    );
    assert_eq!(collect(&output).await.unwrap(), ["first", "second tail"]);
    assert!(stream.0.lock().unwrap().bytes.is_empty());
}

fn idle_error(millis: i64) -> VendorError {
    VendorError::Other(failure(
        FailureKind::Timeout,
        format!("idle timeout {} reached", duration(millis)),
    ))
}

#[tokio::test(start_paused = true)]
async fn exact_idle_boundary_and_removed_margin_fail_d62() {
    let reader = Reader::new(context(), 200, OutputActivity::new(0));
    advance(199).await;
    assert!(!reader.output.is_closed());
    advance(1).await;
    assert!(reader.output.is_closed());
    advance(1).await;
    reader.stream.write("inside the former margin\n");
    assert_eq!(reader.output.receive().await, Err(idle_error(200)));
    assert!(!reader.stream.0.lock().unwrap().bytes.is_empty());
    reader.stop().await;
}

#[tokio::test(start_paused = true)]
async fn ready_bytes_win_over_simultaneous_deadline_and_end() {
    let reader = Reader::new(context(), 200, OutputActivity::new(-200));
    reader.stream.write("ready\ntail");
    reader.end();
    assert_eq!(collect(&reader.output).await.unwrap(), ["ready", "tail"]);
    reader.stop().await;
}

#[tokio::test(start_paused = true)]
async fn idle_deadline_wins_over_end_without_data() {
    let reader = Reader::new(context(), 200, OutputActivity::new(-200));
    reader.end();
    assert_eq!(reader.output.receive().await, Err(idle_error(200)));
    reader.stop().await;
}

async fn once<F: std::future::Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = future;
    std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test(start_paused = true)]
async fn sibling_activity_at_the_old_deadline_invalidates_both_idle_waits() {
    let ctx = context();
    let activity = OutputActivity::new(0);
    let out = LineChannel::new(4);
    let err = LineChannel::new(4);
    let input = Stream::default();
    let mut first = Box::pin(produce_lines(
        Stream::default(),
        out.clone(),
        activity.clone(),
        duration(200),
        ctx.clone(),
        std::future::pending(),
    ));
    let mut second = Box::pin(produce_lines(
        input.clone(),
        err.clone(),
        activity,
        duration(200),
        ctx.clone(),
        std::future::pending(),
    ));
    assert!(once(first.as_mut()).await.is_pending());
    assert!(once(second.as_mut()).await.is_pending());
    tokio::time::advance(std::time::Duration::from_millis(200)).await;
    input.write("activity\n");
    // Gate the two independent continuations explicitly: the positive read wins first.
    assert!(once(second.as_mut()).await.is_pending());
    assert!(once(first.as_mut()).await.is_pending());
    assert!(!out.is_closed());
    assert_eq!(err.receive().await, Ok(Some("activity".into())));
    tokio::time::advance(std::time::Duration::from_millis(199)).await;
    assert!(once(first.as_mut()).await.is_pending());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(once(first.as_mut()).await.is_ready());
    assert!(once(second.as_mut()).await.is_ready());
    assert_eq!(out.receive().await, Err(idle_error(200)));
}

#[tokio::test(start_paused = true)]
async fn eof_stdout_uses_stderr_activity_and_drains_its_tail_only_at_end() {
    let ctx = context();
    let last = OutputActivity::new(0);
    let out = Reader::new(ctx.clone(), 200, last.clone());
    let err = Reader::new(ctx, 200, last);
    out.stream.write("tail");
    out.stream.eof();
    advance(150).await;
    let polls = out.stream.polls();
    err.stream.write("still alive\n");
    assert_eq!(err.output.receive().await, Ok(Some("still alive".into())));
    advance(100).await;
    assert!(!out.output.is_closed());
    assert_eq!(out.stream.polls(), polls);
    out.end();
    err.end();
    assert_eq!(collect(&out.output).await.unwrap(), ["tail"]);
    out.stop().await;
    err.stop().await;
}

#[tokio::test(start_paused = true)]
async fn slow_byte_writes_keep_unicode_and_crlf_whole_and_bursts_preserve_order() {
    let reader = Reader::with_capacity(context(), 200, OutputActivity::new(0), 1);
    for byte in "é😀\r\n".as_bytes() {
        reader.stream.write_bytes(&[*byte]);
        advance(100).await;
    }
    assert_eq!(reader.output.receive().await, Ok(Some("é😀".into())));
    let burst = (0..20_000).map(|n| format!("{n}\n")).collect::<String>();
    reader.stream.write(&burst);
    reader.end();
    assert_eq!(
        collect(&reader.output).await.unwrap(),
        (0..20_000).map(|n| n.to_string()).collect::<Vec<_>>()
    );
    reader.stop().await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_pending_read_eof_wait_and_backpressured_flush() {
    for phase in 0..3 {
        let reader = Reader::with_capacity(context(), 200, OutputActivity::new(0), 1);
        if phase == 1 {
            reader.stream.eof();
        }
        if phase == 2 {
            reader.stream.write("queued\nblocked\ntail");
        }
        tokio::task::yield_now().await;
        reader.scope.cancel(cancelled("stop reader"));
        assert_eq!(reader.task.await.unwrap(), Ok(()));
        assert_eq!(
            reader.output.receive().await,
            Err(VendorError::Cancelled(cancelled("stop reader")))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn consumer_close_or_cancel_stops_a_pending_reader_without_more_reads() {
    for cancel in [false, true] {
        let reader = Reader::new(context(), 200, OutputActivity::new(0));
        tokio::task::yield_now().await;
        let polls = reader.stream.polls();
        let before = tokio::time::Instant::now();
        if cancel {
            reader.output.cancel(cancelled("consumer"));
        } else {
            reader.output.close(None);
        }
        assert_eq!(reader.task.await.unwrap(), Ok(()));
        assert_eq!(reader.stream.polls(), polls);
        assert_eq!(tokio::time::Instant::now(), before);
    }
}

#[tokio::test(start_paused = true)]
async fn backpressured_flush_keeps_its_original_idle_deadline() {
    let ctx = context();
    let stream = Stream::default();
    stream.write("one\ntwo\n");
    let out = LineChannel::new(1);
    let mut task = Box::pin(produce_lines(
        stream,
        out.clone(),
        OutputActivity::new(0),
        duration(200),
        ctx,
        std::future::pending(),
    ));
    assert!(once(task.as_mut()).await.is_pending());
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    assert!(!out.is_closed());
    assert_eq!(out.receive().await, Ok(Some("one".into())));
    assert!(once(task.as_mut()).await.is_ready());
    assert_eq!(out.receive().await, Ok(Some("two".into())));
    assert_eq!(out.receive().await, Err(idle_error(200)));
}

#[tokio::test(start_paused = true)]
async fn signed_idle_limits_and_timestamp_arithmetic_do_not_overflow() {
    for (limit, last) in [(0, 0), (-1, 0), (200, i64::MIN)] {
        let reader = Reader::new(context(), limit, OutputActivity::new(last));
        assert_eq!(reader.output.receive().await, Err(idle_error(limit)));
        reader.stop().await;
    }
    let reader = Reader::new(context(), 200, OutputActivity::new(i64::MAX));
    advance(10_000).await;
    assert!(!reader.output.is_closed());
    reader.stop().await;
}

#[test]
fn vendor_replay_deadline_is_exact_independent_of_wall_hooks_and_runs_off_core() {
    use marathon_core::{
        executor::{
            CoreExecutor,
            hooks::{ClockMode, RuntimeHooks, Tape},
        },
        time::{ReplayDriver, ReplayHooks},
    };
    let result = CoreExecutor::run_virtual_with_watchdog(std::time::Duration::from_secs(60), |core, driver| {
        Box::pin(async move {
            for wall in [
                ClockMode::Frozen(123),
                ClockMode::Step {
                    start: 1000,
                    step: 9999,
                },
            ] {
                let core_thread = std::thread::current().id();
                let clock = core.vendor_clock();
                let start = clock.now_millis();
                let hooks = RuntimeHooks::taped(
                    Tape {
                        wall,
                        ..Tape::default()
                    },
                    core.clock().as_ref(),
                );
                let output = LineChannel::new(4);
                let stream = Stream::default();
                let input = stream.clone();
                let out = output.clone();
                let ctx = VendorContext {
                    clock: clock.clone(),
                    hooks: hooks.clone(),
                    ..context()
                };
                let task = core.vendor().spawn(Box::pin(async move {
                    produce_lines(
                        input,
                        out,
                        OutputActivity::new(start),
                        duration(200),
                        ctx,
                        std::future::pending(),
                    )
                    .await
                }));
                assert_eq!(driver.drain_current().await, Ok(()));
                assert_eq!(clock.now_millis(), start);
                let polling_thread = stream.0.lock().unwrap().poll_thread.expect("reader was polled");
                assert_ne!(polling_thread, core_thread);
                assert_eq!(driver.advance_to(start + 199).await, Ok(()));
                assert!(!output.is_closed());
                assert_eq!(
                    driver
                        .event(start + 200, || {
                            assert!(output.is_closed(), "due timers settle before external input");
                            stream.write("too late\n");
                            Ok(())
                        })
                        .await,
                    Ok(())
                );
                assert_eq!(output.receive().await, Err(idle_error(200)));
                assert_eq!(task.join().await, Ok(()));
                assert_eq!(hooks.verify_consumed(), Ok(()));
            }
            assert_eq!(driver.finish(), Ok(()));
            Ok(())
        })
    });
    assert_eq!(result, Ok(()));
}

#[test]
fn reader_virtual_clock_without_replay_driver_fails_without_host_fallback() {
    use marathon_core::executor::{CoreExecutor, RuntimeClock};
    assert_eq!(
        CoreExecutor::run(|core| Box::pin(async move {
            let output = LineChannel::new(4);
            let out = output.clone();
            let ctx = VendorContext {
                clock: RuntimeClock::virtual_clock().vendor(),
                ..context()
            };
            let task = core.vendor().spawn(Box::pin(produce_lines(
                Stream::default(),
                out,
                OutputActivity::new(0),
                duration(200),
                ctx,
                std::future::pending(),
            )));
            assert_eq!(task.join().await, Ok(()));
            let error = output.receive().await.err().unwrap();
            assert!(
                error
                    .failure()
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("ReplayDriver")
            );
            Ok(())
        })),
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn cancellation_wins_over_ready_bytes_deadline_and_end() {
    let reader = Reader::new(context(), 200, OutputActivity::new(-200));
    reader.stream.write("ready\n");
    reader.end();
    reader.scope.cancel(cancelled("simultaneous stop"));
    assert_eq!(reader.task.await.unwrap(), Ok(()));
    assert_eq!(
        reader.output.receive().await,
        Err(VendorError::Cancelled(cancelled("simultaneous stop")))
    );
    assert_eq!(reader.stream.polls(), 0);
}

#[tokio::test(start_paused = true)]
async fn read_error_retains_the_buffered_tail_and_idle_conversion_overflow_keeps_its_kind() {
    // A stream that yields a partial line and then errors preserves its tail.
    struct PartialThenError(bool);
    impl AsyncRead for PartialThenError {
        fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            if std::mem::replace(&mut self.0, false) {
                buf.put_slice(b"tail");
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(io::Error::other("read failed")))
            }
        }
    }
    impl ReaderInput for PartialThenError {
        fn read_buffered(&mut self, _: &mut [u8]) -> io::Result<Option<usize>> {
            Ok(None)
        }
    }
    let out = LineChannel::new(4);
    assert_eq!(
        produce_lines(
            PartialThenError(true),
            out.clone(),
            OutputActivity::new(0),
            duration(200),
            context(),
            std::future::pending()
        )
        .await,
        Ok(())
    );
    assert_eq!(collect(&out).await.unwrap(), ["tail"]);
    let out = LineChannel::new(4);
    let overflow_input = Stream::default();
    overflow_input.write("ready\ntail");
    assert_eq!(
        produce_lines(
            overflow_input,
            out.clone(),
            OutputActivity::new(0),
            jcompat::jtime::Duration::of_seconds(i64::MAX),
            context(),
            std::future::pending()
        )
        .await,
        Ok(())
    );
    assert_eq!(out.receive().await, Ok(Some("ready".into())));
    assert_eq!(out.receive().await, Ok(Some("tail".into())));
    assert_eq!(
        out.receive().await,
        Err(VendorError::Other(failure(FailureKind::Arithmetic, "long overflow")))
    );
}

#[tokio::test(start_paused = true)]
async fn terminal_drain_does_not_refresh_the_other_streams_idle_deadline() {
    struct FinalOnly(Stream);
    impl AsyncRead for FinalOnly {
        fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl ReaderInput for FinalOnly {
        fn read_buffered(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>> {
            self.0.read_buffered(bytes)
        }
    }
    let ctx = context();
    let activity = OutputActivity::new(0);
    let reader = Reader::new(ctx.clone(), 200, activity.clone());
    advance(100).await;
    let terminal = Stream::default();
    terminal.write("terminal\ntail");
    let out = LineChannel::new(4);
    assert_eq!(
        produce_lines(FinalOnly(terminal), out.clone(), activity, duration(200), ctx, async {}).await,
        Ok(())
    );
    assert_eq!(collect(&out).await.unwrap(), ["terminal", "tail"]);
    advance(99).await;
    assert!(!reader.output.is_closed());
    advance(1).await;
    assert_eq!(reader.output.receive().await, Err(idle_error(200)));
    reader.stop().await;
}

#[tokio::test(start_paused = true)]
async fn stale_readiness_bytes_win_over_simultaneous_idle_deadline_and_end() {
    struct Stale(Stream);
    impl AsyncRead for Stale {
        fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, _: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }
    impl ReaderInput for Stale {
        fn read_buffered(&mut self, bytes: &mut [u8]) -> io::Result<Option<usize>> {
            self.0.read_buffered(bytes)
        }
    }
    let stream = Stream::default();
    stream.write("ready\ntail");
    let out = LineChannel::new(4);
    assert_eq!(
        produce_lines(
            Stale(stream),
            out.clone(),
            OutputActivity::new(-200),
            duration(200),
            context(),
            async {}
        )
        .await,
        Ok(())
    );
    assert_eq!(collect(&out).await.unwrap(), ["ready", "tail"]);
}
