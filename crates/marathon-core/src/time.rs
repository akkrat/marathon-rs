//! Port of core/src/main/kotlin/com/malinskiy/marathon/time/Timer.kt
//! and time/SystemTimer.kt; labelled seams mirror WP 0.2b OracleHooks.

use crate::runtime::{JobRef, LocalFuture, MailboxProbe};
use jcompat::jtime::Instant;
use std::{rc::Rc, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayError {
    pub label: String,
    pub message: String,
}
pub type ClockResult<T> = Result<T, ReplayError>;
pub trait WallClock: Send + Sync {
    fn millis(&self, label: &'static str) -> ClockResult<i64>;
    fn instant(&self, label: &'static str) -> ClockResult<Instant>;
}
pub trait MonotonicClock: Send + Sync {
    fn nanos(&self, label: &'static str) -> ClockResult<i64>;
}
/// Identity token is internal, allocated per object, never a serialized address.
/// WP 3.1 keeps a token registry so repeated object references reuse an ID suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectIdentity(pub u64);
pub trait ReplayHooks: WallClock + MonotonicClock {
    fn draw_int(&self, label: &'static str) -> ClockResult<i32>;
    fn choose_key(&self, label: &'static str, candidates: &[String]) -> ClockResult<String>;
    fn allocate_id(&self, label: &'static str) -> ClockResult<String>;
    fn identity_suffix(&self, label: &'static str, identity: ObjectIdentity) -> ClockResult<String>;
    /// Records accumulated failures and unused explicit tape entries; failures
    /// remain observable even if a caller catches the original error.
    fn verify_consumed(&self) -> ClockResult<()>;
}
pub trait SchedulerClock {
    fn now_millis(&self) -> i64;
    fn delay(&self, millis: i64, job: JobRef) -> LocalFuture<'_, Result<(), crate::error::Cancellation>>;
}
pub trait ReplayDriver {
    fn register_mailbox(&self, label: &'static str, probe: Rc<dyn MailboxProbe>);
    /// Run timers at target and drain all runnable tasks before external delivery.
    fn advance_to(&self, millis: i64) -> LocalFuture<'_, ClockResult<()>>;
    fn drain_current(&self) -> LocalFuture<'_, ClockResult<()>>;
    fn assert_quiescent(&self) -> ClockResult<()>;
    fn finish(&self) -> ClockResult<()>;
}
pub trait BarrierPermit: Send {
    /// Release after operation completion; cancellation before admission has no
    /// permit and must leave the tape cursor unchanged.
    fn finish(self: Box<Self>) -> ClockResult<()>;
}
pub type BarrierFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = ClockResult<Box<dyn BarrierPermit>>> + Send + 'a>>;
pub trait ReplayBarriers: Send + Sync {
    fn enter<'a>(&'a self, label: &'static str, key: &'a String) -> BarrierFuture<'a>;
}
pub trait Timer: Send + Sync {
    fn start_time_millis(&self) -> i64;
    fn elapsed_time_millis(&self) -> ClockResult<i64>;
    fn current_time_millis(&self) -> ClockResult<i64>;
    /// Synchronous nonblocking block only. Exceptions/panics do not take the
    /// second clock reading; wall-time differences use JVM wrapping subtraction.
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64>;
}
pub struct SystemTimer {
    clock: Arc<dyn WallClock>,
    start: i64,
}
impl SystemTimer {
    pub fn new(clock: Arc<dyn WallClock>) -> ClockResult<Self> {
        let start = clock.millis("core.di.Clock")?;
        Ok(Self { clock, start })
    }
}
impl Timer for SystemTimer {
    fn start_time_millis(&self) -> i64 {
        self.start
    }
    fn elapsed_time_millis(&self) -> ClockResult<i64> {
        Ok(self.current_time_millis()?.wrapping_sub(self.start))
    }
    fn current_time_millis(&self) -> ClockResult<i64> {
        self.clock.millis("core.di.Clock")
    }
    fn measure(&self, block: &mut dyn FnMut()) -> ClockResult<i64> {
        let start = self.current_time_millis()?;
        block();
        Ok(self.current_time_millis()?.wrapping_sub(start))
    }
}
