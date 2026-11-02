//! Port of sshj 0.39.0 `net/schmizz/keepalive/{KeepAlive,KeepAliveRunner}.java`, which
//! PerformanceDefaultConfig.kt selects (`KeepAliveProvider.KEEP_ALIVE`) and SshjCommandExecutorFactory.kt configures.
//!
//! Unanswered requests are counted, whatever other traffic arrives (R7, T25): channel output never clears them.

use super::error::{DisconnectReason, SshFailure};
use jcompat::jtime::Duration;
use marathon_core::runtime::{CancellationSignal, VendorClock};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The transport operations the runner uses; tests drive it with scripted replies.
pub trait KeepAliveConnection: Send + Sync {
    /// `conn.getTransport().isRunning()`.
    fn is_running(&self) -> bool;
    /// `conn == conn.getTransport().getService()`: authentication is over.
    fn service_active(&self) -> bool;
    /// `sendGlobalRequest("keepalive@openssh.com", true, …)`; the flag is the promise, set by any reply.
    fn send_keep_alive(&self) -> Arc<AtomicBool>;
    /// `conn.getTransport().die(e)`.
    fn die(&self, cause: SshFailure);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeepAliveRunner {
    /// Whole seconds; disabled unless positive.
    pub keep_alive_interval: i32,
    pub max_alive_count: i32,
}
impl KeepAliveRunner {
    /// `keepAliveInterval.toSeconds().toInt()`: 1500 ms is 1 s, under a second disables it. Approved P7 saturates
    /// where Kotlin's `toInt()` would wrap.
    pub fn new(keep_alive_interval: Duration, max_alive_count: i32) -> Self {
        let seconds = keep_alive_interval.seconds();
        Self {
            keep_alive_interval: seconds.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            max_alive_count,
        }
    }
    pub fn is_enabled(&self) -> bool {
        self.keep_alive_interval > 0
    }
    /// sshj's thread loop: keep alive if the transport runs, then sleep one interval. Returns after `die`.
    pub async fn run(
        &self,
        connection: Arc<dyn KeepAliveConnection>,
        clock: &dyn VendorClock,
        cancellation: Arc<dyn CancellationSignal>,
    ) {
        let mut queue: VecDeque<Arc<AtomicBool>> = VecDeque::new();
        loop {
            if cancellation.reason().is_some() {
                return;
            }
            if connection.is_running() && connection.service_active() {
                while queue.front().is_some_and(|answered| answered.load(Ordering::SeqCst)) {
                    queue.pop_front();
                }
                if queue.len() as i64 >= self.max_alive_count as i64 {
                    connection.die(SshFailure::connection(
                        DisconnectReason::ConnectionLost,
                        format!(
                            "Did not receive any keep-alive response for {} seconds",
                            self.max_alive_count.saturating_mul(self.keep_alive_interval)
                        ),
                    ));
                    return;
                }
                queue.push_back(connection.send_keep_alive());
            }
            if clock
                .delay(self.keep_alive_interval as i64 * 1000, cancellation.clone())
                .await
                .is_err()
            {
                return;
            }
        }
    }
}
