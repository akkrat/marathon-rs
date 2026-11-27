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
use super::supervisor::Lease;
use crate::{cmd::SessionScope, extensions::reader::ReaderInput};
use marathon_core::{
    runtime::{VendorClock, VendorRuntime},
    signal_finish::{SignalDeadline, SignalFinish},
};
use russh::{ChannelId, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig, client};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll, Waker},
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
/// D29: how long an `exec` that failed waits for the peer's reply to its CLOSE. No process runs behind a rejected request,
/// so a healthy peer answers within a round trip, and the slot is free before the caller's next command. U154: the same
/// for a request with no reply, where a peer that has stalled will not answer this either, so waiting longer only adds delay.
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
    stdout_waker: Option<Waker>,
    stderr_waker: Option<Waker>,
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
    /// D93 (round 9): the writer did not send the exec request, because its gate had closed when it came to write it; it
    /// sent CLOSE instead. Nothing was started.
    exec_withheld: Option<marathon_core::error::Cancellation>,
    /// U271: the exec request was sent, and its gate (the caller's cancellation, with its device's gate) closed while the
    /// reply was missing, so the channel was closed. The caller stopped it; the transport did not fail.
    exec_stopped: Option<marathon_core::error::Cancellation>,
    worker_lease: Option<Arc<Lease>>,
    window: u32,
}

/// D93 (round 10): the device's receipt for the command whose exec request was handed to the transport. It ends once the
/// channel has closed (the peer's CLOSE, or, round 11, the transport's actual end: russh's session task has ended, so
/// nothing more reaches the worker over it) or the exec was refused (nothing started). A connection only marked dead does
/// not end it: russh may still send what it holds (an exec queued behind a stalled key exchange) before it disconnects. Shared by the channel, its writer and its pump, so that a channel let go of before its close
/// still ends it when the close comes; one whose close never comes stays live, and its device unconfirmed.
#[derive(Default)]
struct Handed {
    receipt: Option<Arc<crate::cmd::TransferReceipt>>,
    closed: bool,
}
type HandedSlot = Arc<Mutex<Handed>>;
fn hold(slot: &HandedSlot, receipt: Arc<crate::cmd::TransferReceipt>) {
    let mut handed = slot.lock().unwrap_or_else(|e| e.into_inner());
    if handed.closed {
        receipt.end(crate::cmd::TransferCleanup::Confirmed);
    } else {
        handed.receipt = Some(receipt);
    }
}
fn end_handed(slot: &HandedSlot, closed: bool) {
    let mut handed = slot.lock().unwrap_or_else(|e| e.into_inner());
    handed.closed |= closed;
    if let Some(receipt) = handed.receipt.take() {
        receipt.end(crate::cmd::TransferCleanup::Confirmed);
    }
}

/// Test support: what holds an exec request (by its command line) after its device has admitted it and before the
/// transport writes it. It is given a way to declare the connection dead, as the keep-alive does when its replies stop.
#[cfg(feature = "test-support")]
pub type OutboundPause = Arc<
    dyn Fn(&str, Arc<dyn Fn(&str) + Send + Sync>) -> Option<Pin<Box<dyn Future<Output = ()> + Send>>> + Send + Sync,
>;
#[cfg(feature = "test-support")]
static OUTBOUND_PAUSE: Mutex<Option<OutboundPause>> = Mutex::new(None);
/// Test support, D93 (round 10): holds the exec requests `pause` picks between their admission and the transport's write,
/// as russh holds outbound requests during a key exchange. `None` removes it.
#[cfg(feature = "test-support")]
pub fn pause_outbound_exec(pause: Option<OutboundPause>) {
    *OUTBOUND_PAUSE.lock().unwrap_or_else(|e| e.into_inner()) = pause;
}

/// Writes reach the wire in call order: TERM, CLOSE and window adjustments share one queue.
enum Op {
    /// D93 (rounds 9, 10): the exec request, the caller's context the writer looks at as it comes to write it, and the
    /// device's admission, which registers the command with the device as the writer hands it to the transport; if
    /// either refuses, the writer sends CLOSE instead and says so through `withheld`.
    Exec {
        command: String,
        gate: Option<Arc<dyn marathon_core::runtime::CancellationSignal>>,
        admission: Option<Arc<crate::cmd::device_commands::CommandAdmission>>,
        withheld: tokio::sync::oneshot::Sender<marathon_core::error::Cancellation>,
        worker_lease: Option<Arc<Lease>>,
    },
    Signal(Sig),
    Data(Vec<u8>),
    Close,
    Adjust(u32),
}

pub struct ExecChannel {
    signal_finish: OnceLock<Arc<SignalDeadline>>,
    /// sshj's local channel number, `chan#N` in its promise names.
    pub number: u32,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    ops: mpsc::UnboundedSender<Op>,
    shared: Arc<ConnectionShared>,
    clock: Arc<dyn VendorClock>,
    scope: Arc<SessionScope>,
    handed: HandedSlot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Drop for ExecChannel {
    fn drop(&mut self) {
        if let Some(lease) = self.worker_lease() {
            lease.channel_ended();
        }
    }
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
            signal_finish: OnceLock::new(),
            clock: connection.clock.clone(),
            scope: connection.scope(),
            handed: HandedSlot::default(),
        });
        let runtime: &Arc<dyn VendorRuntime> = &connection.runtime;
        runtime.spawn(Box::pin(pump(
            Arc::downgrade(&this),
            read,
            connection.shared.clone(),
            this.handed.clone(),
        )));
        runtime.spawn(Box::pin(writer(
            write,
            queue,
            connection.handle(),
            id,
            connection.shared.clone(),
            this.handed.clone(),
        )));
        this
    }
    fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        let (value, wakers) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let value = f(&mut state);
            (value, [state.stdout_waker.take(), state.stderr_waker.take()])
        };
        self.changed.send_modify(|version| *version += 1);
        for waker in wakers.into_iter().flatten() {
            waker.wake();
        }
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
    pub(super) fn set_worker_lease(&self, lease: Arc<Lease>) {
        self.update(|state| state.worker_lease = Some(lease));
    }
    pub(super) fn worker_lease(&self) -> Option<Arc<Lease>> {
        self.read(|state| state.worker_lease.clone())
    }
    pub(super) async fn protocol_exit(&self) -> Option<i32> {
        self.wait_for(|state| {
            state
                .exit_status
                .map(Some)
                .or_else(|| state.closed.as_ref().map(|_| None))
        })
        .await
    }
    pub(super) async fn data_exit(&self) -> Result<Option<i32>, SshFailure> {
        self.wait_for(|state| {
            if let Some(status) = state.exit_status {
                return Some(Ok(Some(status)));
            }
            if let Some(signal) = state.exit_signal.as_deref() {
                return Some(Ok(super::executor::autodetect_exit_code(Some(signal))));
            }
            state.closed.as_ref().map(|closed| closed.clone().map(|()| None))
        })
        .await
    }
    pub async fn read_ended(&self) {
        self.wait_for(|s| (s.eof || s.close_requested || s.closed.is_some()).then_some(()))
            .await;
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
    /// closes the channel the request was sent on, so that it does not hold a `MaxSessions` slot. U154 does the same for
    /// a request with no reply, with Kotlin's expiry still the error, and one line that names the command.
    pub async fn exec(&self, command: String) -> Result<(), SshFailure> {
        self.exec_gated(command, None, None, false).await
    }
    /// D93 (rounds 9, 10): the exec request is written only if `gate` has not closed and `admission` admits it at the
    /// moment the writer hands it to the transport; the admission registers it with its device in the same step, so a
    /// request the transport sends later (russh holds outbound requests during a key exchange) is the device's live work.
    /// Otherwise the channel is closed instead, nothing is started, and [`ExecChannel::exec_withheld`] says why.
    pub(super) async fn exec_gated(
        &self,
        command: String,
        gate: Option<Arc<dyn marathon_core::runtime::CancellationSignal>>,
        admission: Option<Arc<crate::cmd::device_commands::CommandAdmission>>,
        capture: bool,
    ) -> Result<(), SshFailure> {
        if let Some(admission) = &admission {
            let _ = self.signal_finish.set(admission.signal_finish.clone());
        }
        let worker_lease = self.worker_lease();
        let wire = worker_lease
            .as_ref()
            .map_or_else(|| command.clone(), |lease| lease.wire().to_owned());
        let promise = format!("chan#{} / chanreq for exec", self.number);
        self.update(|s| s.command = command.clone());
        let (withheld, withholding) = tokio::sync::oneshot::channel();
        if self
            .ops
            .send(Op::Exec {
                command: wire,
                gate: gate.clone(),
                admission,
                withheld,
                worker_lease: worker_lease.clone(),
            })
            .is_err()
            || self.shared.is_dead()
        {
            return Err(self.shared.write_failure());
        }
        let withholding = async {
            match withholding.await {
                Ok(reason) => reason,
                Err(_) => std::future::pending().await,
            }
        };
        let replying = within(
            self.clock.as_ref(),
            self.scope.clone(),
            TIMEOUT_MILLIS,
            self.wait_for(|s| match (&s.exec_reply, &s.closed) {
                (Some(accepted), _) => Some(Ok(*accepted)),
                (None, Some(Err(error))) => Some(Err(error.clone())),
                (None, Some(Ok(()))) => Some(Err(SshFailure::connection(DisconnectReason::Unknown, "Request failed"))),
                (None, None) => None,
            }),
        );
        // A device may stop while its exec reply is missing. On supervised workers
        // the token can be ended on the independent connection, even before exec arrives.
        let stopped = async {
            if let Some(gate) = gate.filter(|_| capture || worker_lease.is_some()) {
                return gate.cancelled().await;
            }
            std::future::pending().await
        };
        let reply = tokio::select! { biased;
            reason = withholding => {
                self.update(|s| s.exec_withheld = Some(reason));
                return Err(SshFailure::connection(DisconnectReason::Unknown, "The exec request was not sent"));
            }
            reply = replying => reply,
            reason = stopped => {
                self.update(|s| s.exec_stopped = Some(reason));
                if capture && worker_lease.is_none() {
                    let _ = self.signal(Sig::TERM);
                }
                self.request_close();
                return Err(SshFailure::connection(DisconnectReason::Unknown, "Stopped while waiting for exec reply"));
            }
        };
        let (error, rejected) = match reply {
            Some(Ok(true)) => {
                if let Some(lease) = &worker_lease {
                    lease.acknowledge();
                }
                return Ok(());
            }
            Some(Ok(false)) => (
                SshFailure::connection(DisconnectReason::Unknown, "Request failed"),
                true,
            ),
            Some(Err(error)) => (error, false),
            None => {
                let command = self.read(|s| s.command.clone());
                self.shared.log_executor_error(format!(
                    "No reply to exec `{command}` in 30 s: closing its channel. If the worker gets to the request later, the command may still start, and hold its session slot until it ends"
                ));
                // No TERM ahead of the CLOSE: on a peer that resumes, sshd takes both in one go and its `killpg` beats the new
                // process's `setsid`, so the signal is lost (2 of 30 got through, x21 findings).
                if capture {
                    self.request_close();
                } else {
                    self.close_failed_exec().await;
                }
                return Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT));
            }
        };
        self.shared.loggers.woke_to(&promise, &error);
        if rejected {
            if capture {
                self.request_close();
            } else {
                self.close_failed_exec().await;
            }
        }
        Err(error)
    }
    /// D93 (round 9): why the writer withheld the exec request, if it did (the command did not start).
    pub fn exec_withheld(&self) -> Option<marathon_core::error::Cancellation> {
        self.read(|s| s.exec_withheld.clone())
    }
    /// U271: why the exec's own gate stopped the wait for its reply, if it did. The command may have started; the worker
    /// supervisor ends its token.
    pub fn exec_stopped(&self) -> Option<marathon_core::error::Cancellation> {
        self.read(|s| s.exec_stopped.clone())
    }
    /// D93: the worker answered the exec request with a refusal, so the command did not start.
    pub fn exec_refused(&self) -> bool {
        self.read(|s| s.exec_reply == Some(false))
    }
    /// The caller gets the exec's own error whatever becomes of the CLOSE: its outcome is deliberately unused. A peer that
    /// never answers keeps the slot; the pump drains what it may still send, so the shared connection is not held up.
    async fn close_failed_exec(&self) {
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
    /// `signal(sig)`: no want-reply and no state check, like sshj; a dead transport rejects the write.
    pub fn signal(&self, signal: Sig) -> Result<(), SshFailure> {
        if let Some(lease) = self.worker_lease() {
            lease.signal(&signal);
            return Ok(());
        }
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
    pub(super) fn request_close(&self) {
        if let Some(lease) = self.worker_lease() {
            lease.channel_ended();
        }
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
        if let Some(lease) = self.worker_lease() {
            return self.close_owned(&lease).await;
        }
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
        // Leave one second for the signal and CLOSE to reach the worker before receipt expiry.
        let killed = self
            .bounded_close(TERM_WAIT_MILLIS, |finish| finish.receipts.saturating_sub(1_000), ended)
            .await
            .is_none();
        if killed {
            // A dead transport rejects the write, and the wait below ends with its error.
            let _ = self.signal(Sig::KILL);
        }
        if self.read(|s| s.closed.is_none()) {
            self.request_close();
        }
        let closed = self
            .bounded_close(
                CLOSE_WAIT_MILLIS,
                |finish| finish.receipts,
                self.wait_for(|s| s.closed.clone()),
            )
            .await;
        if closed.is_none() {
            let command = self.update(|s| {
                s.abandoned = true;
                if s.closed.is_none() {
                    self.shared.abandon_session();
                }
                s.command.clone()
            });
            let wait = if self.has_signal_deadline() {
                "before the shutdown deadline"
            } else {
                "in 30 s"
            };
            self.shared.log_executor_error(format!(
                "Abandoning session `{command}`: no reply to TERM, KILL and CLOSE {wait}. The process may still run on the worker and hold its session slot"
            ));
        } else if killed {
            // U183: a server may ignore both signals and still confirm the close, so the close alone proves nothing.
            let command = self.read(|s| s.command.clone());
            let wait = if self.has_signal_deadline() {
                "before the shutdown escalation deadline"
            } else {
                "within 20 s of TERM"
            };
            self.shared.log_session_close_warning(format!(
                "Session `{command}` did not end {wait} and was closed after KILL. The process may still be running on the worker"
            ));
        }
        self.close_outcome(closed)
    }

    fn has_signal_deadline(&self) -> bool {
        [self.signal_finish.get(), self.shared.provider_signal.get()]
            .into_iter()
            .flatten()
            .any(|signal| signal.current().is_some())
    }

    async fn bounded_close<T>(
        &self,
        millis: i64,
        cutoff: fn(SignalFinish) -> i64,
        work: impl Future<Output = T>,
    ) -> Option<T> {
        self.close_window(cutoff, self.bounded(millis, work)).await
    }

    async fn close_window<T>(
        &self,
        cutoff: fn(SignalFinish) -> i64,
        timed: impl Future<Output = Option<T>>,
    ) -> Option<T> {
        async fn wait(signal: Option<&Arc<SignalDeadline>>, clock: &dyn VendorClock, cutoff: fn(SignalFinish) -> i64) {
            match signal {
                Some(signal) => signal.wait_for_signal(clock, cutoff).await,
                None => std::future::pending().await,
            }
        }
        tokio::select! { biased;
            result = timed => result,
            _ = wait(self.signal_finish.get(), self.clock.as_ref(), cutoff) => None,
            _ = wait(self.shared.provider_signal.get(), self.clock.as_ref(), cutoff) => None,
        }
    }

    async fn close_owned(&self, lease: &Lease) -> Result<(), SshFailure> {
        let wait = |millis| {
            within(
                self.clock.as_ref(),
                Arc::new(SessionScope::default()),
                millis,
                lease.proof(),
            )
        };
        let already_closing = self.update(|s| std::mem::replace(&mut s.escalating, true));
        let first_wait = if already_closing {
            TIMEOUT_MILLIS
        } else {
            TERM_WAIT_MILLIS
        };
        let proof = match self
            .close_window(|finish| finish.receipts.saturating_sub(1_000), wait(first_wait))
            .await
        {
            None if !already_closing || self.has_signal_deadline() => {
                lease.signal(&Sig::KILL);
                self.close_window(|finish| finish.receipts, wait(CLOSE_WAIT_MILLIS))
                    .await
            }
            result => result,
        };
        // A wedged data channel can be discarded once worker cleanup is
        // settled. A channel CLOSE does not settle the worker receipt.
        self.request_close();
        match proof {
            Some(Ok(_)) => Ok(()),
            Some(Err(error)) => Err(SshFailure::connection(
                DisconnectReason::Unknown,
                error.failure().to_string(),
            )),
            None => Err(SshFailure::timeout_expired(CONNECTION_EXCEPTION, TIMEOUT_TEXT)),
        }
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

    pub(super) fn write_data(&self, data: Vec<u8>) -> Result<(), SshFailure> {
        if !self.is_open() || self.shared.is_dead() || self.ops.send(Op::Data(data)).is_err() {
            return Err(self.shared.write_failure());
        }
        Ok(())
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
            ChannelMsg::ExitStatus { exit_status } => {
                let status = exit_status as i32;
                if let Some(lease) = self.worker_lease() {
                    lease.data_status(status);
                }
                self.update(|s| s.exit_status = Some(status));
            }
            ChannelMsg::ExitSignal { signal_name, .. } => {
                self.update(|s| s.exit_signal = Some(signal_text(&signal_name)))
            }
            ChannelMsg::Success => self.update(|s| s.exec_reply = s.exec_reply.or(Some(true))),
            ChannelMsg::Failure => {
                // D93 (round 10): a refused exec started nothing.
                end_handed(&self.handed, false);
                self.update(|s| s.exec_reply = s.exec_reply.or(Some(false)))
            }
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
        if let Some(lease) = self.worker_lease() {
            lease.channel_ended();
        }
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
    handed: HandedSlot,
) -> marathon_core::error::VendorResult<()> {
    // D93 (round 11): set once the connection is marked dead. Its callers hear of the death at once, but the pump goes on
    // draining until russh's session task has really ended (the read half's end) or the peer's CLOSE comes, and only
    // then ends the command's receipt: until then russh may still send an exec it holds.
    let mut dead = false;
    loop {
        let message = if dead {
            read.wait().await
        } else {
            tokio::select! {
                biased;
                _ = shared.wait_dead() => {
                    dead = true;
                    if let Some(target) = channel.upgrade() {
                        target.finish(Err(shared.channel_failure()));
                    }
                    continue;
                }
                message = read.wait() => message,
            }
        };
        // A peer that has not answered the CLOSE of a dropped channel may still send: keep draining until it answers
        // or the transport ends, so that russh's queue never holds up the connection.
        let Some(target) = channel.upgrade() else {
            if matches!(message, None | Some(ChannelMsg::Close)) {
                end_handed(&handed, true);
                return Ok(());
            }
            continue;
        };
        match message {
            Some(message) => {
                if !target.deliver(message) {
                    end_handed(&handed, true);
                    return Ok(());
                }
            }
            None => {
                target.finish(Err(shared.channel_failure()));
                end_handed(&handed, true);
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
    handed: HandedSlot,
) -> marathon_core::error::VendorResult<()> {
    while let Some(op) = queue.recv().await {
        if shared.is_dead() {
            break;
        }
        // Write errors mean the transport is gone; the pump reports that to the channel.
        let _ = match op {
            Op::Exec {
                command,
                gate,
                admission,
                withheld,
                worker_lease,
            } => {
                // D93 (rounds 9, 10): the last look before the command is handed to the transport, and its registration
                // with the device in the same step as the device's gate is looked at.
                let admitted = match gate.as_ref().and_then(|gate| gate.reason()) {
                    Some(reason) => Err(reason),
                    None => admission.map_or(Ok(None), |admission| admission.hand_over()),
                };
                match admitted {
                    Err(reason) => {
                        let _ = withheld.send(reason);
                        write.close().await
                    }
                    Ok(receipt) => {
                        if let Some(lease) = worker_lease {
                            lease.start(receipt);
                        } else if let Some(receipt) = receipt {
                            hold(&handed, receipt);
                        }
                        #[cfg(feature = "test-support")]
                        {
                            let pause = OUTBOUND_PAUSE.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            let dying = shared.clone();
                            let die: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |why| {
                                dying.die(SshFailure::connection(DisconnectReason::ConnectionLost, why));
                            });
                            if let Some(paused) = pause.and_then(|pause| pause(&command, die)) {
                                paused.await;
                            }
                        }
                        write.exec(true, command).await
                    }
                }
            }
            Op::Signal(signal) => write.signal(signal).await,
            Op::Data(data) => write.data_bytes(data).await,
            Op::Close => write.close().await,
            Op::Adjust(bytes) => handle.adjust_window(id, bytes).await,
        };
    }
    Ok(())
}

/// A waiting stream over the pump's buffer. Buffered bytes always precede EOF.
pub struct ChannelReader {
    channel: Arc<ExecChannel>,
    stream: Stream,
}
impl AsyncRead for ChannelReader {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        // Register before inspecting data/end, so an update between the two cannot be missed.
        {
            let mut state = self.channel.state.lock().unwrap_or_else(|e| e.into_inner());
            let waker = match self.stream {
                Stream::Stdout => &mut state.stdout_waker,
                Stream::Stderr => &mut state.stderr_waker,
            };
            *waker = Some(cx.waker().clone());
        }
        if self.channel.take(self.stream, buf) > 0
            || self.channel.read(|s| {
                let empty = match self.stream {
                    Stream::Stdout => s.stdout.is_empty(),
                    Stream::Stderr => s.stderr.is_empty(),
                };
                empty && (s.eof || s.close_requested || s.closed.is_some())
            })
        {
            return Poll::Ready(Ok(()));
        }
        Poll::Pending
    }
}
impl ReaderInput for ChannelReader {
    fn read_buffered(&mut self, bytes: &mut [u8]) -> std::io::Result<Option<usize>> {
        Ok(Some(self.channel.take(self.stream, &mut ReadBuf::new(bytes))))
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::{PerformanceDefaultConfig, SshLogging};
    use super::super::error::is_instance;
    use super::*;
    use marathon_core::runtime::{CancellationSignal, VendorFuture, VendorTimeoutBody};
    use marathon_logging::LogSettings;

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

    #[derive(Default)]
    struct NoClock(Option<watch::Sender<i64>>);
    impl VendorClock for NoClock {
        fn now_millis(&self) -> i64 {
            self.0.as_ref().map_or(0, |now| *now.borrow())
        }
        fn delay(&self, millis: i64, _: Arc<dyn CancellationSignal>) -> VendorFuture<'_, ()> {
            let Some(now) = &self.0 else {
                return Box::pin(std::future::pending());
            };
            let at = self.now_millis() + millis;
            Box::pin(async move {
                now.subscribe().wait_for(|now| *now >= at).await.unwrap();
                Ok(())
            })
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

    fn reader_test_channel() -> Arc<ExecChannel> {
        let logging = SshLogging::new(Arc::new(Mutex::new(LogSettings::default())), Arc::new(|_, _, _, _| {}));
        Arc::new(ExecChannel {
            signal_finish: OnceLock::new(),
            number: 0,
            state: Mutex::new(State {
                window: WINDOW_SIZE,
                ..Default::default()
            }),
            changed: watch::channel(0).0,
            ops: mpsc::unbounded_channel().0,
            shared: ConnectionShared::new(PerformanceDefaultConfig::new(false, logging).loggers()),
            clock: Arc::new(NoClock::default()),
            scope: Arc::new(SessionScope::default()),
            handed: HandedSlot::default(),
        })
    }

    #[tokio::test]
    async fn a_signal_tightens_an_ssh_close_already_waiting_on_term_d97() {
        let mut channel = reader_test_channel();
        let (now, _) = watch::channel(0);
        let (ops, mut writes) = mpsc::unbounded_channel();
        let inner = Arc::get_mut(&mut channel).unwrap();
        inner.ops = ops;
        inner.clock = Arc::new(NoClock(Some(now.clone())));
        let signal = Arc::new(SignalDeadline::default());
        assert!(inner.signal_finish.set(signal.clone()).is_ok());

        channel.signal(Sig::TERM).unwrap();
        assert!(matches!(writes.try_recv(), Ok(Op::Signal(Sig::TERM))));
        let close = channel.close_escalating();
        tokio::pin!(close);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(close.as_mut().poll(&mut cx).is_pending());
        now.send_replace(1_000);
        signal.set(SignalFinish::from_start(1_000));
        assert!(close.as_mut().poll(&mut cx).is_pending());
        now.send_replace(17_999);
        assert!(close.as_mut().poll(&mut cx).is_pending());
        assert!(writes.try_recv().is_err());
        now.send_replace(18_000);
        assert!(close.as_mut().poll(&mut cx).is_pending());
        assert!(matches!(writes.try_recv(), Ok(Op::Signal(Sig::KILL))));
        assert!(matches!(writes.try_recv(), Ok(Op::Close)));
        now.send_replace(19_000);
        assert!(matches!(close.as_mut().poll(&mut cx), Poll::Ready(Err(_))));
    }

    fn end_reader(channel: &ExecChannel, event: usize) {
        match event {
            0 => {
                channel.deliver(ChannelMsg::Eof);
            }
            1 => {
                channel.deliver(ChannelMsg::Close);
            }
            2 => channel.request_close(),
            _ => channel.finish(Err(SshFailure::new(CONNECTION_EXCEPTION, "transport lost"))),
        }
    }

    #[test]
    fn ssh_readers_wake_for_data_eof_close_requested_close_and_transport_loss() -> std::io::Result<()> {
        use std::{
            sync::atomic::{AtomicUsize, Ordering},
            task::Wake,
        };
        struct Wakes(AtomicUsize);
        impl Wake for Wakes {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        for event in 0..4 {
            let channel = reader_test_channel();
            let mut out = channel.reader(Stream::Stdout);
            let mut err = channel.reader(Stream::Stderr);
            let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
            let waker = Waker::from(wakes.clone());
            let mut cx = Context::from_waker(&waker);
            let mut bytes = [0; 64];
            for reader in [&mut out, &mut err] {
                assert!(
                    Pin::new(reader)
                        .poll_read(&mut cx, &mut ReadBuf::new(&mut bytes))
                        .is_pending()
                );
            }
            channel.receive(Stream::Stdout, b"stdout");
            assert_eq!(wakes.0.load(Ordering::SeqCst), 2);
            channel.receive(Stream::Stderr, b"stderr");
            // Drain data, then suspend both readers again before the terminal transition.
            for (reader, expected) in [(&mut out, b"stdout"), (&mut err, b"stderr")] {
                let mut buf = ReadBuf::new(&mut bytes);
                assert!(matches!(
                    Pin::new(&mut *reader).poll_read(&mut cx, &mut buf),
                    Poll::Ready(Ok(()))
                ));
                assert_eq!(buf.filled(), expected);
                assert!(
                    Pin::new(reader)
                        .poll_read(&mut cx, &mut ReadBuf::new(&mut bytes))
                        .is_pending()
                );
            }
            let before = wakes.0.load(Ordering::SeqCst);
            end_reader(&channel, event);
            assert_eq!(wakes.0.load(Ordering::SeqCst), before + 2);
            for reader in [&mut out, &mut err] {
                let mut buf = ReadBuf::new(&mut bytes);
                assert!(matches!(
                    Pin::new(reader).poll_read(&mut cx, &mut buf),
                    Poll::Ready(Ok(()))
                ));
                assert!(buf.filled().is_empty());
            }
            // The terminal signal is latched even when subscribed after delivery.
            let mut end = std::pin::pin!(channel.read_ended());
            assert!(end.as_mut().poll(&mut cx).is_ready());
        }
        Ok(())
    }

    #[tokio::test]
    async fn terminal_ssh_data_precedes_eof_close_and_transport_loss_without_idle_ticks() {
        use crate::{
            cmd::channel::LineChannel,
            extensions::reader::{OutputActivity, produce_lines},
        };
        use marathon_core::{executor::hooks::RuntimeHooks, vendor::VendorContext};
        for event in 0..4 {
            let channel = reader_test_channel();
            let output = LineChannel::new(4);
            let ctx = VendorContext {
                clock: Arc::new(NoClock::default()),
                hooks: RuntimeHooks::live(),
                cancellation: Arc::new(SessionScope::default()),
            };
            let ended = channel.clone();
            let mut reader = Box::pin(produce_lines(
                channel.reader(Stream::Stdout),
                output.clone(),
                OutputActivity::new(0),
                jcompat::jtime::duration_of(i64::MAX / 1000, (i64::MAX % 1000) * 1_000_000).unwrap(),
                ctx,
                async move { ended.read_ended().await },
            ));
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(reader.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            // Gate the pump between the reader's Pending and its continuation.
            channel.receive(Stream::Stdout, b"first\npartial");
            end_reader(&channel, event);
            assert_eq!(reader.await, Ok(()));
            assert_eq!(output.receive().await, Ok(Some("first".into())));
            assert_eq!(output.receive().await, Ok(Some("partial".into())));
            assert_eq!(output.receive().await, Ok(None));
            if event != 2 {
                channel.receive(Stream::Stdout, b"after end");
                assert_eq!(channel.available(Stream::Stdout), 0);
            }
            let mut empty = [];
            assert_eq!(
                channel.reader(Stream::Stdout).read_buffered(&mut empty).ok(),
                Some(Some(0))
            );
        }
    }

    #[test]
    fn a_peer_that_ignores_the_window_still_hits_the_cap() {
        // A conforming peer cannot reach this any more, and the fake honours the window, so the channel is driven
        // directly: memory stays bounded and the failure stays sshj's.
        let logging = SshLogging::new(Arc::new(Mutex::new(LogSettings::default())), Arc::new(|_, _, _, _| {}));
        let shared = ConnectionShared::new(PerformanceDefaultConfig::new(false, logging).loggers());
        let channel = ExecChannel {
            signal_finish: OnceLock::new(),
            number: 0,
            state: Mutex::new(State {
                window: WINDOW_SIZE,
                ..Default::default()
            }),
            changed: watch::channel(0).0,
            ops: mpsc::unbounded_channel().0,
            shared: shared.clone(),
            clock: Arc::new(NoClock::default()),
            scope: Arc::new(SessionScope::default()),
            handed: HandedSlot::default(),
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
        assert!(is_instance(&death.kind, CIRCULAR_BUFFER_EXCEPTION));
        assert_eq!(death.message().as_deref(), Some("Attempted overflow"));
    }
}
