//! D92: one shutdown deadline for the device's requests, producers and remote cleanup.

use marathon_adb::client::SocketFactory;
use marathon_config::vendor::AndroidTimeoutConfiguration;
use marathon_core::{
    error::*,
    executor::shared::{Signal, requested},
    runtime::*,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, AtomicUsize, Ordering},
};
use tokio::sync::Notify;

pub(crate) const FORCE_RESERVE_MILLIS: i64 = 5_000;
const RUNNING: i64 = i64::MIN;

pub(crate) fn allowance(c: &AndroidTimeoutConfiguration) -> i64 {
    let idle = c.socket_idle_timeout;
    let idle = (idle.to_millis() != Some(marathon_adb::client::transport::NO_IDLE_LIMIT)).then_some(idle);
    [c.shell, c.install, c.pull_file]
        .iter()
        .copied()
        .chain(idle)
        .map(|d| {
            // Compute before clamping so a negative or overflowing Duration cannot wrap to a new timeout.
            let millis = i128::from(d.seconds()) * 1_000 + i128::from(d.nanos()) / 1_000_000;
            // The clock reserves i64::MAX for an infinite delay. Leave D84 room for its 5 s margin as well.
            let largest = i64::MAX - FORCE_RESERVE_MILLIS - 1;
            i64::try_from(millis.clamp(0, i128::from(largest))).unwrap_or(largest)
        })
        .fold(FORCE_RESERVE_MILLIS, i64::max)
}

pub(crate) struct DeviceWork {
    requests: Arc<SocketFactory>,
    deadline: AtomicI64,
    active: Arc<Active>,
    pub(crate) force: Arc<Signal>,
    pub(crate) cleanup: Mutex<Option<Arc<dyn VendorTask>>>,
}

#[derive(Default)]
struct Active {
    count: AtomicUsize,
    changed: Notify,
}

pub(crate) struct Work(Arc<Active>);

impl Drop for Work {
    fn drop(&mut self) {
        if self.0.count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.changed.notify_waiters();
        }
    }
}

impl DeviceWork {
    pub(crate) fn new(requests: Arc<SocketFactory>) -> Self {
        Self {
            requests,
            deadline: AtomicI64::new(RUNNING),
            active: Arc::default(),
            force: Signal::new(),
            cleanup: Mutex::default(),
        }
    }
    pub(crate) fn seal(&self, deadline: i64) {
        let _ = self
            .deadline
            .compare_exchange(RUNNING, deadline, Ordering::SeqCst, Ordering::SeqCst);
        self.requests.seal();
    }

    pub(crate) fn is_sealed(&self) -> bool {
        self.requests.is_sealed()
    }

    pub(crate) fn deadline(&self) -> Option<i64> {
        let deadline = self.deadline.load(Ordering::SeqCst);
        (deadline != RUNNING).then_some(deadline)
    }

    pub(crate) fn ensure_running(&self) -> VendorResult<()> {
        if self.is_sealed() {
            return Err(VendorError::Cancelled(requested("Android device is stopping")));
        }
        Ok(())
    }

    pub(crate) fn admit(&self) -> VendorResult<Work> {
        // The count and the ADB seal share a sequentially consistent order. Every admitted
        // operation is visible to idle() after sealing, including before the task is spawned.
        self.active.count.fetch_add(1, Ordering::SeqCst);
        let work = Work(self.active.clone());
        self.ensure_running()?;
        Ok(work)
    }

    pub(crate) fn spawn(&self, runtime: &dyn VendorRuntime, body: VendorTaskBody) -> VendorResult<Arc<dyn VendorTask>> {
        let work = self.admit()?;
        Ok(self.spawn_admitted(runtime, work, body))
    }

    pub(crate) fn spawn_admitted(
        &self,
        runtime: &dyn VendorRuntime,
        work: Work,
        body: VendorTaskBody,
    ) -> Arc<dyn VendorTask> {
        let force = self.force.clone();
        runtime.spawn_cancellable(Box::new(move |signal| {
            Box::pin(async move {
                let _work = work;
                body(Arc::new(ChildSignal(signal, force))).await
            })
        }))
    }

    pub(crate) async fn idle(&self) {
        loop {
            let changed = self.active.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.active.count.load(Ordering::SeqCst) == 0 {
                break;
            }
            changed.await;
        }
    }
}

/// Cancelling the owner must wake producers even when their caller no longer has a waiter.
pub(crate) struct ChildSignal(pub Arc<dyn CancellationSignal>, pub Arc<dyn CancellationSignal>);

impl CancellationSignal for ChildSignal {
    fn reason(&self) -> Option<Cancellation> {
        self.0.reason().or_else(|| self.1.reason())
    }

    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Cancellation> + Send + '_>> {
        Box::pin(async move {
            tokio::select! { biased; reason = self.0.cancelled() => reason, reason = self.1.cancelled() => reason }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcompat::jtime::{Duration, duration_of};
    use marathon_config::vendor::{AndroidTimeoutParameters, android_timeout_configuration};

    #[test]
    fn cleanup_allowance_has_a_finite_floor_and_saturates_d92() {
        let mut timeouts = android_timeout_configuration(AndroidTimeoutParameters::empty());
        assert_eq!(allowance(&timeouts), 60_000);
        timeouts.shell = Duration::of_seconds(1);
        timeouts.install = Duration::of_seconds(-2);
        timeouts.pull_file = Duration::ZERO;
        timeouts.socket_idle_timeout = Duration::ZERO;
        assert_eq!(allowance(&timeouts), FORCE_RESERVE_MILLIS);
        timeouts.socket_idle_timeout = Duration::of_seconds(42);
        assert_eq!(allowance(&timeouts), 42_000);
        let no_idle = marathon_adb::client::transport::NO_IDLE_LIMIT;
        timeouts.socket_idle_timeout = duration_of(no_idle / 1_000, (no_idle % 1_000) * 1_000_000).unwrap();
        assert_eq!(allowance(&timeouts), FORCE_RESERVE_MILLIS);
        timeouts.install = Duration::of_seconds(i64::MAX);
        assert_eq!(allowance(&timeouts), i64::MAX - FORCE_RESERVE_MILLIS - 1);
    }
}
