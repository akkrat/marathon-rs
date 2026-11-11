//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/XcodebuildSession.kt

use marathon_logging::Level;

use crate::{
    cmd::{CommandSession, SessionScope, cancelled, failure, millis, remote::ssh::sshj::config::SshLogging},
    test::TestEvent,
};
use jcompat::jtime::Duration;
use marathon_core::{
    error::{Failure, VendorError, VendorResult},
    runtime::{CancellationSignal, VendorRuntime, VendorTask},
    vendor::VendorContext,
};
use marathon_error::FailureKind;
use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

pub const CLOSE_TIMEOUT: Duration = Duration::of_seconds(60);

#[derive(Default)]
struct Events {
    queue: VecDeque<Vec<TestEvent>>,
    end: Option<VendorResult<()>>,
    cancelled: bool,
}
#[cfg(feature = "test-support")]
static SETTLE_EVENT_LINES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Select a valid A20 interleaving for the hermetic differential probe only.
#[cfg(feature = "test-support")]
pub fn settle_event_lines_for_differential() {
    SETTLE_EVENT_LINES.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg_attr(not(feature = "test-support"), derive(Default))]
struct Channel {
    state: Mutex<Events>,
    changed: Notify,
    #[cfg(feature = "test-support")]
    settled: Option<tokio::sync::Semaphore>,
}
#[cfg(feature = "test-support")]
impl Default for Channel {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: Notify::new(),
            #[cfg(feature = "test-support")]
            settled: SETTLE_EVENT_LINES
                .load(std::sync::atomic::Ordering::Relaxed)
                .then(|| tokio::sync::Semaphore::new(0)),
        }
    }
}
#[derive(Clone)]
pub struct EventSender(Arc<Channel>);
pub struct EventReceiver(Arc<Channel>);
impl EventSender {
    pub async fn send(&self, events: Vec<TestEvent>) -> VendorResult<()> {
        loop {
            let changed = self.0.changed.notified();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.cancelled || state.end.is_some() {
                    return Err(VendorError::Cancelled(cancelled("Channel was closed")));
                }
                // Approved S1: one event list can run ahead of the reader.
                if state.queue.is_empty() {
                    state.queue.push_back(events);
                    self.0.changed.notify_waiters();
                    return Ok(());
                }
            }
            changed.await;
        }
    }
    pub async fn send_line<F>(&self, events: Vec<TestEvent>, line: F) -> VendorResult<()>
    where
        F: Future<Output = VendorResult<()>>,
    {
        #[cfg(feature = "test-support")]
        if let Some(handled) = &self.0.settled {
            // The native golden pins line-before-handler, one valid A20 ordering.
            // Acknowledgement replaces elapsed-time guesses between event handlers.
            line.await?;
            self.send(events).await?;
            loop {
                let changed = self.0.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.0.state.lock().unwrap_or_else(|e| e.into_inner()).cancelled {
                    return Err(VendorError::Cancelled(cancelled("Channel was cancelled")));
                }
                tokio::select! {
                    permit = handled.acquire() => {
                        if let Ok(permit) = permit { permit.forget(); }
                        return Ok(());
                    }
                    _ = changed => {}
                }
            }
        }
        // Production retains A20's event-before-line ordering.
        self.send(events).await?;
        line.await
    }
    fn finish(&self, result: VendorResult<()>) -> bool {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled || state.end.is_some() {
            return false;
        }
        state.end = Some(result);
        self.0.changed.notify_waiters();
        true
    }
}
impl EventReceiver {
    #[cfg(feature = "test-support")]
    pub fn processed(&self) {
        if let Some(handled) = &self.0.settled {
            handled.add_permits(1);
        }
    }
    pub async fn receive(&self, context: &VendorContext) -> VendorResult<Option<Vec<TestEvent>>> {
        loop {
            let changed = self.0.changed.notified();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(events) = state.queue.pop_front() {
                    self.0.changed.notify_waiters();
                    return Ok(Some(events));
                }
                if let Some(result) = &state.end {
                    return result.clone().map(|()| None);
                }
            }
            tokio::select! { biased;
                reason = context.cancellation.cancelled() => return Err(VendorError::Cancelled(reason)),
                _ = changed => (),
            }
        }
    }
    pub fn cancel(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.cancelled = true;
        state.queue.clear();
        // kotlinx channels retain the first close cause; cancellation only discards queued events.
        state
            .end
            .get_or_insert_with(|| Err(VendorError::Cancelled(cancelled("Channel was cancelled"))));
        self.0.changed.notify_waiters();
    }
}

#[derive(Default)]
struct State {
    started: bool,
    stopped: bool,
    finished: bool,
    abandoned: bool,
    close_failure: Option<Failure>,
    task: Option<Arc<dyn VendorTask>>,
    observation: Option<SessionObservation>,
}
#[derive(Clone)]
pub(crate) struct SessionObservation {
    pub elapsed_millis: i64,
    pub command_opened: bool,
    pub termination: Option<crate::cmd::base_command::CommandTermination>,
    pub output_timed_out: bool,
}
pub struct XcodebuildSession {
    state: Mutex<State>,
    changed: Notify,
    cancellation: Arc<SessionScope>,
    runtime: Arc<dyn VendorRuntime>,
    logging: SshLogging,
}
impl XcodebuildSession {
    pub fn new(runtime: Arc<dyn VendorRuntime>, logging: SshLogging) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            changed: Notify::new(),
            cancellation: Arc::default(),
            runtime,
            logging,
        })
    }
    pub(crate) fn observation(&self) -> Option<SessionObservation> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).observation.clone()
    }
    pub fn closed(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.stopped && (!s.started || s.finished) && s.close_failure.is_none()
    }
    pub fn abandoned(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).abandoned
    }
    pub fn check_closed(&self, serial: &str) -> VendorResult<()> {
        if self.closed() {
            return Ok(());
        }
        let cause = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close_failure
            .clone();
        let mut detail = failure(
            FailureKind::IllegalState,
            format!("Previous xcodebuild session on {serial} has not been closed"),
        );
        detail.cause = cause.map(Box::new);
        Err(device_lost(detail))
    }
    pub fn produce<O, OF, C, CF>(self: &Arc<Self>, context: VendorContext, open: O, consume: C) -> EventReceiver
    where
        O: FnOnce(VendorContext) -> OF + Send + 'static,
        OF: Future<Output = VendorResult<CommandSession>> + Send,
        C: FnOnce(Arc<CommandSession>, EventSender, VendorContext) -> CF + Send + 'static,
        CF: Future<Output = VendorResult<()>> + Send,
    {
        let channel = Arc::new(Channel::default());
        let sender = EventSender(channel.clone());
        let this = self.clone();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.started = true;
        let task = self.runtime.spawn(Box::pin(async move {
            let mut ctx = context;
            ctx.cancellation = this.cancellation.clone();
            let started = ctx.clock.now_millis();
            let clock = ctx.clock.clone();
            // The opener owns transport acquisition cleanup; never drop it at cancellation.
            let opened = if let Some(reason) = this.cancellation.reason() {
                Err(VendorError::Cancelled(reason))
            } else {
                open(ctx.clone()).await
            };
            let mut command = None;
            let result = match opened {
                Ok(value) => {
                    let value = Arc::new(value);
                    command = Some(value.clone());
                    tokio::select! { biased;
                        reason=this.cancellation.cancelled()=>Err(VendorError::Cancelled(reason)),
                        result=consume(value, sender.clone(), ctx)=>result,
                    }
                }
                Err(error) => Err(error),
            };
            this.state.lock().unwrap_or_else(|e| e.into_inner()).observation = Some(SessionObservation {
                elapsed_millis: clock.now_millis().saturating_sub(started).max(0),
                command_opened: command.is_some(),
                output_timed_out: command.is_some()
                    && result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.failure().kind == FailureKind::Timeout),
                termination: command
                    .as_ref()
                    .and_then(|command| command.base.exit_code.termination()),
            });
            // Non-cancellation failures reach the reader before close. A producer cancellation
            // is rethrown in Kotlin, so its channel completes only after finally.
            let cancellation = matches!(result, Err(VendorError::Cancelled(_)));
            if !cancellation
                && !sender.finish(result.clone())
                && let Err(error) = &result
                && !matches!(error, VendorError::Cancelled(_))
            {
                let logger = this.logging.component("marathon_apple::xcodebuild_session");
                if this.logging.enabled(&logger, Level::DEBUG) {
                    (this.logging.sink)(
                        &logger,
                        Level::DEBUG,
                        "xcodebuild session failed after its events were no longer read".into(),
                        Some(error.failure().clone()),
                    );
                }
            }
            let close_failure = if let Some(command) = command {
                command.close().await.err().map(|e| e.failure().clone())
            } else {
                None
            };
            {
                let mut state = this.state.lock().unwrap_or_else(|e| e.into_inner());
                state.close_failure = close_failure;
                state.finished = true;
            }
            if cancellation {
                sender.finish(result);
            }
            this.changed.notify_waiters();
            Ok(())
        }));
        state.task = Some(task);
        EventReceiver(channel)
    }
    async fn joined(&self) {
        loop {
            let changed = self.changed.notified();
            {
                let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if s.stopped && (!s.started || s.finished) {
                    return;
                }
            }
            changed.await;
        }
    }
    async fn wait(&self, timeout: Duration, context: &VendorContext, protected: bool) -> VendorResult<bool> {
        let parent: Arc<dyn CancellationSignal> = if protected {
            Arc::new(SessionScope::default())
        } else {
            context.cancellation.clone()
        };
        Ok(context
            .clock
            .with_timeout_or_null(
                parent,
                millis(timeout)?,
                Box::new(|signal| {
                    Box::pin(async move {
                        tokio::select! { biased;
                            reason=signal.cancelled()=>Err(VendorError::Cancelled(reason)),
                            _=self.joined()=>Ok(()),
                        }
                    })
                }),
            )
            .await?
            .is_some())
    }
    pub async fn stop(&self, timeout: Duration, context: &VendorContext) -> Option<Failure> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
        self.cancellation.cancel(cancelled("Job was cancelled"));
        self.changed.notify_waiters();
        let result = match self.wait(timeout, context, true).await {
            Ok(true) => self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .close_failure
                .clone(),
            Ok(false) => Some(failure(
                FailureKind::Timeout,
                format!("xcodebuild session has not closed within {timeout}"),
            )),
            Err(error) => Some(error.failure().clone()),
        };
        if result.is_some() {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).abandoned = true;
        }
        result
    }
    pub async fn await_closed(&self, timeout: Duration, context: &VendorContext) -> VendorResult<bool> {
        self.wait(timeout, context, false).await?;
        Ok(self.closed())
    }
    /// Device disposal may join a late close. Stop's timeout never detaches/aborts its producer.
    pub async fn join_producer(&self) -> VendorResult<()> {
        let task = self.state.lock().unwrap_or_else(|e| e.into_inner()).task.clone();
        if let Some(task) = task {
            task.join().await?;
        }
        Ok(())
    }
}
pub(crate) fn device_lost(cause: Failure) -> VendorError {
    let mut detail = failure(FailureKind::DeviceLost, cause.to_string());
    detail.cause = Some(Box::new(cause));
    VendorError::DeviceLost(detail)
}

#[cfg(all(test, feature = "test-support"))]
mod line_order_tests {
    use super::*;
    use std::task::Poll;

    async fn once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        let mut future = future;
        std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
    }

    #[tokio::test]
    async fn differential_line_gate_waits_for_line_then_handler_and_cancel_releases_it() {
        let channel = Arc::new(Channel {
            settled: Some(tokio::sync::Semaphore::new(0)),
            ..Channel::default()
        });
        let sender = EventSender(channel.clone());
        let receiver = EventReceiver(channel.clone());
        let (line, delivered) = tokio::sync::oneshot::channel();
        let mut send = Box::pin(sender.send_line(vec![TestEvent::TestRunStartedEvent], async move {
            assert!(delivered.await.is_ok());
            Ok(())
        }));
        assert!(once(send.as_mut()).await.is_pending());
        assert!(channel.state.lock().unwrap().queue.is_empty());
        assert!(line.send(()).is_ok());
        assert!(once(send.as_mut()).await.is_pending());
        assert_eq!(channel.state.lock().unwrap().queue.len(), 1);
        // Receiving alone is not the acknowledgement: callbacks must finish first.
        assert!(channel.state.lock().unwrap().queue.pop_front().is_some());
        assert!(once(send.as_mut()).await.is_pending());
        receiver.processed();
        assert_eq!(send.await, Ok(()));
        let mut send = Box::pin(sender.send_line(vec![TestEvent::TestRunEnded], async { Ok(()) }));
        assert!(once(send.as_mut()).await.is_pending());
        receiver.cancel();
        assert!(matches!(send.await, Err(VendorError::Cancelled(_))));
    }

    #[tokio::test]
    async fn production_line_delivery_keeps_event_before_line_without_acknowledgement() {
        let channel = Arc::new(Channel {
            settled: None,
            ..Channel::default()
        });
        let sender = EventSender(channel.clone());
        let seen = channel.clone();
        assert_eq!(
            sender
                .send_line(vec![TestEvent::TestRunStartedEvent], async move {
                    assert_eq!(seen.state.lock().unwrap().queue.len(), 1);
                    Ok(())
                })
                .await,
            Ok(())
        );
    }
}
