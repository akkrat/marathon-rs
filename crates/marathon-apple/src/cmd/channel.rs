//! Port of vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/cmd/CommandSession.kt
//! Kotlin channel close preserves queued lines; cancellation discards them.

use super::cancelled;
use marathon_core::error::{Cancellation, Failure, VendorError, VendorResult};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

#[derive(Clone)]
pub struct LineChannel(Arc<Inner>);
struct Inner {
    state: Mutex<State>,
    changed: Notify,
    capacity: usize,
}
struct State {
    queue: VecDeque<String>,
    end: End,
}
#[derive(Clone)]
enum End {
    Open,
    Closed(Option<Failure>),
    Cancelled(Cancellation),
}
impl LineChannel {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "line channel must be bounded with positive capacity");
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                end: End::Open,
            }),
            changed: Notify::new(),
            capacity,
        }))
    }
    pub fn is_closed(&self) -> bool {
        !matches!(self.0.state.lock().unwrap_or_else(|e| e.into_inner()).end, End::Open)
    }
    pub async fn closed(&self) {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_closed() {
                return;
            }
            changed.await;
        }
    }
    pub fn close(&self, cause: Option<Failure>) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state.end, End::Open) {
            state.end = End::Closed(cause);
        }
        drop(state);
        self.0.changed.notify_waiters();
    }
    pub fn cancel(&self, reason: Cancellation) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.queue.clear();
        // kotlinx channels keep the first close cause; cancel only discards the queued lines then.
        if matches!(state.end, End::Open) {
            state.end = End::Cancelled(reason);
        }
        drop(state);
        self.0.changed.notify_waiters();
    }
    pub async fn send(&self, line: String) -> VendorResult<()> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                match &state.end {
                    End::Closed(Some(f)) => return Err(VendorError::Other(f.clone())),
                    End::Closed(None) => return Err(VendorError::Cancelled(cancelled("Channel closed"))),
                    End::Cancelled(c) => return Err(VendorError::Cancelled(c.clone())),
                    End::Open => {}
                }
                if state.queue.len() < self.0.capacity {
                    state.queue.push_back(line);
                    drop(state);
                    self.0.changed.notify_waiters();
                    return Ok(());
                }
            }
            changed.await;
        }
    }
    pub async fn receive(&self) -> VendorResult<Option<String>> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(line) = state.queue.pop_front() {
                    drop(state);
                    self.0.changed.notify_waiters();
                    return Ok(Some(line));
                }
                match &state.end {
                    End::Open => {}
                    End::Closed(None) => return Ok(None),
                    End::Closed(Some(f)) => return Err(VendorError::Other(f.clone())),
                    End::Cancelled(c) => return Err(VendorError::Cancelled(c.clone())),
                }
            }
            changed.await;
        }
    }
}
