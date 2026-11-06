//! Port of vendor/vendor-apple/base/src/main/kotlin/sshj/connection/channel/direct/PatchedExecChannel.kt
//! and of sshj 0.39.0 `AbstractChannel`/`SessionChannel`/`ChannelInputStream`, which it extends.
//!
//! A pump drains russh's channel messages at once into per-stream byte buffers, so the session loop never waits
//! on a consumer. The receive window grows only as the readers consume (`ChannelInputStream.checkWindow`, with
//! the vendored russh's manual window): a stalled consumer stops its own channel, not the connection.

use super::client::{
    ConnectionShared, HandleRef, MAX_PACKET_SIZE, SshConnection, TIMEOUT_MILLIS, TIMEOUT_TEXT, WINDOW_SIZE, within,
};
use super::error::{CIRCULAR_BUFFER_EXCEPTION, CONNECTION_EXCEPTION, DisconnectReason, SshFailure};
use crate::cmd::SessionScope;
use jcompat::logging::Level;
use marathon_core::runtime::{VendorClock, VendorRuntime};
use russh::{ChannelId, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig, client};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::{mpsc, watch},
};

/// `Window.Local` threshold: `min(maxPacketSize * 20, initialWinSize / 4)`.
const ADJUST_THRESHOLD: u32 = if MAX_PACKET_SIZE * 20 < WINDOW_SIZE / 4 {
    MAX_PACKET_SIZE * 20
} else {
    WINDOW_SIZE / 4
};
/// `ConfigImpl.maxCircularBufferSize`: each stream's buffer cap, which bounds the window adjustments.
const MAX_BUFFER: usize = 16 * 1024 * 1024;
/// D29: how long a rejected `exec` waits for the peer's reply to its CLOSE. No process runs behind a rejected request,
/// so a healthy peer answers within a round trip, and the slot is free before the caller's next command.
const REJECTED_CLOSE_WAIT_MILLIS: i64 = 5_000;
/// D30: how long a process gets to end on TERM before KILL and CLOSE. Most of Kotlin's 30 s, so xcodebuild has room to
/// finish its cancellation (how long that takes on a real worker is unmeasured). A server that ignores signals gets its
/// CLOSE this much later, but marathon cannot drive a worker through such a server anyway.
const TERM_WAIT_MILLIS: i64 = 20_000;
/// D30: how long the peer gets to answer CLOSE, which after KILL needs only a reap. With [`TERM_WAIT_MILLIS`] this is
/// sshj's 30 s, so no close takes longer than Kotlin's first wait, and `Timeout expired: 30000 MILLISECONDS` stays true.
const CLOSE_WAIT_MILLIS: i64 = TIMEOUT_MILLIS - TERM_WAIT_MILLIS;

#[derive(Default)]
struct State {
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    eof: bool,
    close_requested: bool,
    /// D30: a [`ExecChannel::close_escalating`] has begun; later callers wait for it instead of repeating it.
    escalating: bool,
    /// D30: the escalation gave up on the peer. A late CLOSE still sets `closed`.
    abandoned: bool,
    /// The exec string, for the line that reports an abandoned session.
    command: String,
    /// `closeEvent`: set by the peer's CLOSE, or failed with the transport's error.
    closed: Option<Result<(), SshFailure>>,
    exit_status: Option<i32>,
    exit_signal: Option<String>,
    exec_reply: Option<bool>,
    window: u32,
}

/// Writes reach the wire in call order: TERM, CLOSE and window adjustments share one queue.
enum Op {
    Exec(String),
    Signal(Sig),
    Close,
    Adjust(u32),
}

pub struct ExecChannel {
    /// sshj's local channel number, `chan#N` in its promise names.
    pub number: u32,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    ops: mpsc::UnboundedSender<Op>,
    shared: Arc<ConnectionShared>,
    clock: Arc<dyn VendorClock>,
    scope: Arc<SessionScope>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl ExecChannel {
    pub(crate) fn start(number: u32, channel: russh::Channel<client::Msg>, connection: &SshConnection) -> Arc<Self> {
        let id = channel.id();
        let (read, write) = channel.split();
        let (ops, queue) = mpsc::unbounded_channel();
        let this = Arc::new(Self {
            number,
            state: Mutex::new(State {
                window: WINDOW_SIZE,
                ..Default::default()
            }),
            changed: watch::channel(0).0,
            ops,
            shared: connection.shared.clone(),
            clock: connection.clock.clone(),
            scope: connection.scope(),
        });
        let runtime: &Arc<dyn VendorRuntime> = &connection.runtime;
        runtime.spawn(Box::pin(pump(Arc::downgrade(&this), read, connection.shared.clone())));
        runtime.spawn(Box::pin(writer(
            write,
            queue,
            connection.handle(),
            id,
            connection.shared.clone(),
        )));
        this
    }
    fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let value = f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
        self.changed.send_modify(|version| *version += 1);
        value
    }
    fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }
    async fn wait_for<T>(&self, mut ready: impl FnMut(&State) -> Option<T>) -> T {
        let mut rx = self.changed.subscribe();
        loop {
            if let Some(value) = self.read(&mut ready) {
                return value;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
    /// `isOpen()`: open, no CLOSE received or failure delivered, no CLOSE requested.
    pub fn is_open(&self) -> bool {
        self.read(|s| s.closed.is_none() && !s.close_requested)
    }
    pub fn is_eof(&self) -> bool {
        self.read(|s| s.eof)
    }
    pub fn exit_status(&self) -> Option<i32> {
        self.read(|s| s.exit_status)
    }
    pub fn exit_signal(&self) -> Option<String> {
        self.read(|s| s.exit_signal.clone())
    }
    /// `ChannelInputStream.available()`: received bytes of `stream` not read yet.
    pub fn available(&self, stream: Stream) -> usize {
        self.read(|s| match stream {
            Stream::Stdout => s.stdout.len(),
            Stream::Stderr => s.stderr.len(),
        })
    }
    /// `getLocalWinSize()`: bytes the peer may still send before the next window adjustment.
    pub fn local_win_size(&self) -> u32 {
        self.read(|s| s.window)
    }
    /// `exec` with want-reply under the 30 s request timeout; a CHANNEL_FAILURE is `Request failed` (T23), and D29
    /// closes the channel the request was sent on, so that it does not hold a `MaxSessions` slot.
    pub async fn exec(&self, command: String) -> Result<(), SshFailure> {
        let promise = format!("chan#{} / chanreq for exec", self.number);
        self.update(|s| s.command = command.clone());
        if self.ops.send(Op::Exec(command)).is_err() || self.shared.is_dead() {
            return Err(self.shared.write_failure());
        }
        let reply = within(
            self.clock.as_ref(),
            self.scope.clone(),
            TIMEOUT_MILLIS,
            self.wait_for(|s| match (&s.exec_reply, &s.closed) {
                (Some(accepted), _) => Some(Ok(*accepted)),
                (None, Some(Err(error))) => Some(Err(error.clone())),
                (None, Some(Ok(()))) => Some(Err(SshFailure::connection(DisconnectReason::Unknown, "Request failed"))),
                (None, None) => None,
            }),
        )
        .await;
        let (error, rejected) = match reply {
            Some(Ok(true)) => return Ok(()),
            Some(Ok(false)) => (
                SshFailure::connection(DisconnectReason::Unknown, "Request failed"),
                true,
            ),
            Some(Err(error)) => (error, false),
            None => return Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT)),
        };
        self.shared.loggers.woke_to(&promise, &error);
        if rejected {
            // The caller gets the rejection whatever becomes of the CLOSE: its outcome is deliberately unused.
            self.request_close();
            let closing = self.wait_for(|s| s.closed.clone());
            let _ = within(
                self.clock.as_ref(),
                self.scope.clone(),
                REJECTED_CLOSE_WAIT_MILLIS,
                closing,
            )
            .await;
        }
        Err(error)
    }
    /// `signal(sig)`: no want-reply and no state check, like sshj; a dead transport rejects the write.
    pub fn signal(&self, signal: Sig) -> Result<(), SshFailure> {
        if self.shared.is_dead() || self.ops.send(Op::Signal(signal)).is_err() {
            return Err(self.shared.write_failure());
        }
        Ok(())
    }
    /// `AbstractChannel.close()`: if open, request CLOSE and wait up to 30 s for the peer's; an error delivered
    /// meanwhile is rethrown, and expiry is `ConnectionException(TimeoutException)`.
    pub async fn close(&self) -> Result<(), SshFailure> {
        if !self.is_open() {
            return Ok(());
        }
        self.request_close();
        let result = within(
            self.clock.as_ref(),
            self.scope.clone(),
            TIMEOUT_MILLIS,
            self.wait_for(|s| s.closed.clone()),
        )
        .await;
        match result {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => {
                self.shared
                    .loggers
                    .woke_to(&format!("chan#{} / close", self.number), &error);
                Err(error)
            }
            None => Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT)),
        }
    }
    /// `sendClose`: `closeRequested` is set even if the write fails.
    fn request_close(&self) {
        let first = self.update(|s| !std::mem::replace(&mut s.close_requested, true));
        if first {
            let _ = self.ops.send(Op::Close);
        }
    }
    /// D30, replacing `close()` and `join()` on the way out of a session (A14). The caller has sent TERM. The process
    /// gets [`TERM_WAIT_MILLIS`] to end, then KILL; then CLOSE, and [`CLOSE_WAIT_MILLIS`] for the peer's reply, after which
    /// the session is abandoned and this fails like Kotlin's `close()` did.
    ///
    /// KILL comes before CLOSE because russh forgets a channel when it sends CLOSE, and drops any later signal. Signals are
    /// advisory (some SSH servers ignore them), so only this bound guarantees progress.
    ///
    /// A later or concurrent call never starts over and waits no longer than the bound, and a dead transport ends any wait
    /// at once. An abandoned channel stays open on the peer, in its slot, until the process ends or the connection does:
    /// the connection is shared with other sessions, so it is left alone.
    pub async fn close_escalating(&self) -> Result<(), SshFailure> {
        if let Some(result) = self.read(|s| s.closed.clone()) {
            return result;
        }
        if self.update(|s| std::mem::replace(&mut s.escalating, true)) {
            let settled = self.wait_for(|s| match (&s.closed, s.abandoned) {
                (Some(result), _) => Some(Some(result.clone())),
                (None, true) => Some(None),
                (None, false) => None,
            });
            return self.close_outcome(self.bounded(TIMEOUT_MILLIS, settled).await.flatten());
        }
        let ended =
            self.wait_for(|s| (s.closed.is_some() || s.exit_status.is_some() || s.exit_signal.is_some()).then_some(()));
        if self.bounded(TERM_WAIT_MILLIS, ended).await.is_none() {
            // A dead transport rejects the write, and the wait below ends with its error.
            let _ = self.signal(Sig::KILL);
        }
        if self.read(|s| s.closed.is_none()) {
            self.request_close();
        }
        let closed = self
            .bounded(CLOSE_WAIT_MILLIS, self.wait_for(|s| s.closed.clone()))
            .await;
        if closed.is_none() {
            let command = self.update(|s| {
                s.abandoned = true;
                s.command.clone()
            });
            let loggers = &self.shared.loggers;
            let logger = loggers
                .logging
                .logger("com.malinskiy.marathon.apple.cmd.remote.ssh.sshj.SshjCommandExecutor");
            loggers.logging.log(
                &logger,
                Level::Error,
                || {
                    format!(
                        "Abandoning session `{command}`: no reply to TERM, KILL and CLOSE in 30 s. The process may still run on the worker and hold its session slot"
                    )
                },
                None,
            );
        }
        self.close_outcome(closed)
    }
    /// D30: a close has begun, so its TERM has been sent.
    pub fn is_closing(&self) -> bool {
        self.read(|s| s.escalating)
    }
    async fn bounded<T>(&self, millis: i64, future: impl Future<Output = T>) -> Option<T> {
        within(self.clock.as_ref(), self.scope.clone(), millis, future).await
    }
    fn close_outcome(&self, closed: Option<Result<(), SshFailure>>) -> Result<(), SshFailure> {
        match closed {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => {
                self.shared
                    .loggers
                    .woke_to(&format!("chan#{} / close", self.number), &error);
                Err(error)
            }
            None => Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT)),
        }
    }
    /// `join(1, NANOSECONDS)`.
    pub fn join_poll(&self) -> Result<(), SshFailure> {
        self.read(|s| s.closed.clone())
            .unwrap_or_else(|| Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, "1 NANOSECONDS")))
    }
    pub fn reader(self: &Arc<Self>, stream: Stream) -> ChannelReader {
        ChannelReader {
            channel: self.clone(),
            stream,
        }
    }
    fn deliver(&self, message: ChannelMsg) -> bool {
        match message {
            ChannelMsg::Data { data } => self.receive(Stream::Stdout, &data),
            ChannelMsg::ExtendedData { data, ext: 1 } => self.receive(Stream::Stderr, &data),
            ChannelMsg::ExtendedData { ext, .. } => {
                // sshj's reader thread dies with this exception: the whole connection is lost.
                self.shared.die(SshFailure::connection(
                    DisconnectReason::ProtocolError,
                    format!("Bad extended data type = {ext}"),
                ));
            }
            ChannelMsg::Eof => self.update(|s| s.eof = true),
            ChannelMsg::ExitStatus { exit_status } => self.update(|s| s.exit_status = Some(exit_status as i32)),
            ChannelMsg::ExitSignal { signal_name, .. } => {
                self.update(|s| s.exit_signal = Some(signal_text(&signal_name)))
            }
            ChannelMsg::Success => self.update(|s| s.exec_reply = s.exec_reply.or(Some(true))),
            ChannelMsg::Failure => self.update(|s| s.exec_reply = s.exec_reply.or(Some(false))),
            ChannelMsg::Close => {
                self.finish(Ok(()));
                return false;
            }
            _ => {}
        }
        true
    }
    /// Data after EOF or close is dropped: PatchedExecChannel mutes "Getting data on EOF'ed stream".
    ///
    /// `CircularBuffer.ensureCapacity`: a stream buffer holds at most `MAX_BUFFER - 1` bytes. `window_credit` keeps a
    /// peer that honours the window from getting here (D37). One that does not is still stopped, as in sshj: the
    /// connection dies with the exception, and the rejected packet is not buffered.
    fn receive(&self, stream: Stream, data: &[u8]) {
        let overflow = self.update(|s| {
            s.window = s.window.saturating_sub(data.len() as u32);
            if s.eof || s.closed.is_some() {
                return false;
            }
            let buffer = match stream {
                Stream::Stdout => &mut s.stdout,
                Stream::Stderr => &mut s.stderr,
            };
            if buffer.len() + data.len() + 1 > MAX_BUFFER {
                return true;
            }
            buffer.extend(data);
            #[cfg(feature = "test-support")]
            self.shared.note_unread(buffer.len());
            false
        });
        if overflow {
            self.shared
                .die(SshFailure::new(CIRCULAR_BUFFER_EXCEPTION, "Attempted overflow"));
        }
    }
    fn finish(&self, result: Result<(), SshFailure>) {
        self.update(|s| {
            if s.closed.is_none() {
                s.closed = Some(result);
            }
        })
    }
    /// `ChannelInputStream.read` + `checkWindow`: either stream's read refills the shared window once it is low.
    fn take(&self, stream: Stream, buf: &mut ReadBuf<'_>) -> usize {
        let (count, adjustment) = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let source = match stream {
                Stream::Stdout => &mut s.stdout,
                Stream::Stderr => &mut s.stderr,
            };
            let count = source.len().min(buf.remaining());
            let (head, tail) = source.as_slices();
            let first = head.len().min(count);
            buf.put_slice(&head[..first]);
            buf.put_slice(&tail[..count - first]);
            source.drain(..count);
            let bytes = window_credit(s.window, s.stdout.len().max(s.stderr.len()));
            let adjustment = (count > 0 && bytes > 0).then(|| {
                s.window += bytes;
                bytes
            });
            (count, adjustment)
        };
        if let Some(bytes) = adjustment {
            let _ = self.ops.send(Op::Adjust(bytes));
        }
        count
    }
}

/// How far a read may grow the window. D37: the peer may spend the window on either stream, so it is capped by the
/// fuller one, not by the stream just read (A32). When that stream is unread the window runs out, the worker blocks
/// and the other stream stops too. What ends that: a read of that stream, the command's timeout or an idle timeout,
/// or the session's close, since TERM and CLOSE need no window. The cap only bites above `MAX_BUFFER - WINDOW_SIZE`
/// unread bytes; below it every grant is what sshj's would be.
fn window_credit(window: u32, fullest: usize) -> u32 {
    let needed = if window <= ADJUST_THRESHOLD {
        WINDOW_SIZE - window
    } else {
        0
    };
    // CircularBuffer.maxPossibleRemainingCapacity: `maxSize - available - 1`.
    let capacity = MAX_BUFFER as i64 - fullest as i64 - 1;
    (needed as i64).min(capacity - window as i64).max(0) as u32
}

fn signal_text(signal: &Sig) -> String {
    match signal {
        Sig::ABRT => "ABRT",
        Sig::ALRM => "ALRM",
        Sig::FPE => "FPE",
        Sig::HUP => "HUP",
        Sig::ILL => "ILL",
        Sig::INT => "INT",
        Sig::KILL => "KILL",
        Sig::PIPE => "PIPE",
        Sig::QUIT => "QUIT",
        Sig::SEGV => "SEGV",
        Sig::TERM => "TERM",
        Sig::USR1 => "USR1",
        Sig::Custom(name) => name,
    }
    .to_owned()
}

/// Drains russh's channel queue without waiting on any consumer; a dead transport fails the channel
/// (`notifyError`) with sshj's chained error.
async fn pump(
    channel: std::sync::Weak<ExecChannel>,
    mut read: ChannelReadHalf,
    shared: Arc<ConnectionShared>,
) -> marathon_core::error::VendorResult<()> {
    loop {
        let message = tokio::select! {
            biased;
            _ = shared.wait_dead() => None,
            message = read.wait() => message,
        };
        // A peer that has not answered the CLOSE of a dropped channel may still send: keep draining until it answers
        // or the transport ends, so that russh's queue never holds up the connection.
        let Some(target) = channel.upgrade() else {
            if matches!(message, None | Some(ChannelMsg::Close)) {
                return Ok(());
            }
            continue;
        };
        match message {
            Some(message) => {
                if !target.deliver(message) {
                    return Ok(());
                }
            }
            None => {
                target.finish(Err(shared.channel_failure()));
                return Ok(());
            }
        }
    }
}

async fn writer(
    write: ChannelWriteHalf<client::Msg>,
    mut queue: mpsc::UnboundedReceiver<Op>,
    handle: HandleRef,
    id: ChannelId,
    shared: Arc<ConnectionShared>,
) -> marathon_core::error::VendorResult<()> {
    while let Some(op) = queue.recv().await {
        if shared.is_dead() {
            break;
        }
        // Write errors mean the transport is gone; the pump reports that to the channel.
        let _ = match op {
            Op::Exec(command) => write.exec(true, command).await,
            Op::Signal(signal) => write.signal(signal).await,
            Op::Close => write.close().await,
            Op::Adjust(bytes) => handle.adjust_window(id, bytes).await,
        };
    }
    Ok(())
}

/// One stream for 4.1's `produce_lines_manually`. An empty buffer is "available() == 0": no data, no EOF;
/// the reader's `can_read` predicate (`isOpen && !isEOF`) ends it.
pub struct ChannelReader {
    channel: Arc<ExecChannel>,
    stream: Stream,
}
impl AsyncRead for ChannelReader {
    fn poll_read(self: Pin<&mut Self>, _: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.channel.take(self.stream, buf) == 0 {
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{PerformanceDefaultConfig, SshLogging};
    use super::super::error::is_instance;
    use super::*;
    use jcompat::logging::MarathonLogging;
    use marathon_core::runtime::{CancellationSignal, VendorFuture, VendorTimeoutBody};

    /// sshj's grant, which looked only at the stream just read.
    fn sshj_credit(window: u32, read_stream: usize) -> u32 {
        let needed = if window <= ADJUST_THRESHOLD {
            WINDOW_SIZE - window
        } else {
            0
        };
        (needed as i64)
            .min(MAX_BUFFER as i64 - read_stream as i64 - 1 - window as i64)
            .max(0) as u32
    }

    #[test]
    fn grants_are_unchanged_while_neither_stream_is_near_its_cap_d37() {
        let last_ordinary = MAX_BUFFER - WINDOW_SIZE as usize - 1;
        for window in (0..=WINDOW_SIZE)
            .step_by(4096)
            .chain([ADJUST_THRESHOLD, ADJUST_THRESHOLD + 1])
        {
            for fullest in (0..=last_ordinary).step_by(64 * 1024).chain([last_ordinary]) {
                for read_stream in [0, fullest / 2, fullest] {
                    assert_eq!(
                        window_credit(window, fullest),
                        sshj_credit(window, read_stream),
                        "window {window}, fullest {fullest}, read stream {read_stream}"
                    );
                }
            }
        }
    }

    #[test]
    fn grants_never_let_the_unread_stream_overflow_d37() {
        let cap = MAX_BUFFER as u64 - 1;
        for unread in [
            cap - 1,
            cap - 100,
            cap - ADJUST_THRESHOLD as u64,
            cap - WINDOW_SIZE as u64 + 1,
        ] {
            let unread = unread as usize;
            // Everything already granted may still arrive on the unread stream.
            let window = (cap - unread as u64).min(ADJUST_THRESHOLD as u64) as u32;
            let credit = window_credit(window, unread);
            assert!(unread as u64 + u64::from(window) + u64::from(credit) <= cap);
            // sshj's grant for a read of the other, empty stream goes past the cap.
            assert!(unread as u64 + u64::from(window) + u64::from(sshj_credit(window, 0)) > cap);
        }
        assert_eq!(window_credit(0, MAX_BUFFER - 1), 0);
        assert_eq!(
            window_credit(0, MAX_BUFFER - 1 - 16_384),
            16_384,
            "reading it reopens the window"
        );
        assert_eq!(window_credit(0, 0), WINDOW_SIZE);
    }

    struct NoClock;
    impl VendorClock for NoClock {
        fn delay(&self, _: i64, _: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
            Box::pin(std::future::pending())
        }
        fn with_timeout<'a>(
            &'a self,
            _: Arc<dyn CancellationSignal>,
            _: i64,
            _: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, ()> {
            Box::pin(std::future::pending())
        }
        fn with_timeout_or_null<'a>(
            &'a self,
            _: Arc<dyn CancellationSignal>,
            _: i64,
            _: VendorTimeoutBody<'a>,
        ) -> VendorFuture<'a, Option<()>> {
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn a_peer_that_ignores_the_window_still_hits_the_cap() {
        // A conforming peer cannot reach this any more, and the fake honours the window, so the channel is driven
        // directly: memory stays bounded and the failure stays sshj's.
        let logging = SshLogging::new(
            Arc::new(Mutex::new(MarathonLogging::default())),
            Arc::new(|_, _, _, _| {}),
        );
        let shared = ConnectionShared::new(PerformanceDefaultConfig::new(false, logging).loggers());
        let channel = ExecChannel {
            number: 0,
            state: Mutex::new(State {
                window: WINDOW_SIZE,
                ..Default::default()
            }),
            changed: watch::channel(0).0,
            ops: mpsc::unbounded_channel().0,
            shared: shared.clone(),
            clock: Arc::new(NoClock),
            scope: Arc::new(SessionScope::default()),
        };
        let packet = vec![b'x'; MAX_PACKET_SIZE as usize];
        let fit = (MAX_BUFFER - 1) / packet.len();
        for _ in 0..fit {
            channel.receive(Stream::Stdout, &packet);
        }
        assert!(!shared.is_dead());
        channel.receive(Stream::Stdout, &packet);
        assert_eq!(channel.available(Stream::Stdout), fit * packet.len(), "not buffered");
        let death = shared.death().unwrap();
        assert!(is_instance(death.class, CIRCULAR_BUFFER_EXCEPTION));
        assert_eq!(death.message().as_deref(), Some("Attempted overflow"));
    }
}
