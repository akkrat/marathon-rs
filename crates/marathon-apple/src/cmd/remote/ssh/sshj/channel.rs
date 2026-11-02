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
use marathon_core::runtime::{VendorClock, VendorRuntime};
use russh::{ChannelId, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig, client};
use std::{
    collections::VecDeque,
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

#[derive(Default)]
struct State {
    stdout: VecDeque<u8>,
    stderr: VecDeque<u8>,
    eof: bool,
    close_requested: bool,
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
    /// `exec` with want-reply under the 30 s request timeout; a CHANNEL_FAILURE is `Request failed` (T23).
    pub async fn exec(&self, command: String) -> Result<(), SshFailure> {
        let promise = format!("chan#{} / chanreq for exec", self.number);
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
        let error = match reply {
            Some(Ok(true)) => return Ok(()),
            Some(Ok(false)) => SshFailure::connection(DisconnectReason::Unknown, "Request failed"),
            Some(Err(error)) => error,
            None => return Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT)),
        };
        self.shared.loggers.woke_to(&promise, &error);
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
    /// `join()`: no timeout.
    pub async fn join(&self) -> Result<(), SshFailure> {
        let error = match self.read(|s| s.closed.clone()) {
            Some(result) => return result,
            None => self.wait_for(|s| s.closed.clone()).await,
        };
        if let Err(error) = &error {
            self.shared
                .loggers
                .woke_to(&format!("chan#{} / close", self.number), error);
        }
        error
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
    /// `CircularBuffer.ensureCapacity`: a stream buffer holds at most `MAX_BUFFER - 1` bytes. Reads on the other
    /// stream refill the shared window regardless, so an unread stream can overflow; sshj's reader thread then dies
    /// with the exception and takes the whole connection with it. The rejected packet is not buffered.
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
    /// `ChannelInputStream.read` + `checkWindow`: either stream's read refills the shared window once it is low,
    /// by no more than the read stream's buffer can still take (`maxPossibleRemainingCapacity - window`), so a
    /// slow consumer of one stream holds at most `MAX_BUFFER - 1` unread bytes; `receive` enforces the cap.
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
            let available = source.len();
            let needed = if s.window <= ADJUST_THRESHOLD {
                WINDOW_SIZE - s.window
            } else {
                0
            };
            // CircularBuffer.maxPossibleRemainingCapacity: `maxSize - available - 1`.
            let capacity = MAX_BUFFER as i64 - available as i64 - 1;
            let bytes = (needed as i64).min(capacity - s.window as i64);
            let adjustment = (count > 0 && bytes > 0).then(|| {
                s.window += bytes as u32;
                bytes as u32
            });
            (count, adjustment)
        };
        if let Some(bytes) = adjustment {
            let _ = self.ops.send(Op::Adjust(bytes));
        }
        count
    }
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
        // A rejected exec leaves its channel open on the peer (A33): keep draining until the transport ends.
        let Some(target) = channel.upgrade() else {
            if message.is_none() {
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
