//! Port of core/src/test/kotlin/com/malinskiy/marathon/device/DeviceFeatureTest.kt
//! and time/SystemTimerTest.kt.

use jcompat::jtime::Instant;
use marathon_core::{
    model::DeviceFeature,
    time::{ClockResult, SystemTimer, Timer, WallClock},
};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

#[test]
fn should_parse_valid_uppercase_string() {
    assert_eq!(
        DeviceFeature::from_string(Some("VIDEO")),
        Ok(Some(DeviceFeature::Video))
    );
}
#[test]
fn should_parse_valid_lowercase_string() {
    assert_eq!(
        DeviceFeature::from_string(Some("screenshot")),
        Ok(Some(DeviceFeature::Screenshot))
    );
}
#[test]
fn should_throw_exception_for_invalid_string() {
    assert!(DeviceFeature::from_string(Some("key let off")).is_err());
}
struct Clock {
    now: AtomicI64,
    step: i64,
}
impl WallClock for Clock {
    fn millis(&self, label: &'static str) -> ClockResult<i64> {
        assert_eq!(label, "core.di.Clock");
        Ok(self.now.fetch_add(self.step, Ordering::SeqCst))
    }
    fn instant(&self, _label: &'static str) -> ClockResult<Instant> {
        unreachable!("SystemTimer uses millis, not instant")
    }
}
#[test]
fn should_call_passed_clock_to_get_current_time_millis() -> ClockResult<()> {
    let clock = Arc::new(Clock {
        now: AtomicI64::new(0),
        step: 0,
    });
    let timer = SystemTimer::new(clock.clone())?;
    clock.now.store(100, Ordering::SeqCst);
    assert_eq!(timer.current_time_millis()?, 100);
    Ok(())
}
#[test]
fn should_call_passed_clock_to_measure() -> ClockResult<()> {
    let clock = Arc::new(Clock {
        now: AtomicI64::new(0),
        step: 1000,
    });
    let timer = SystemTimer::new(clock.clone())?;
    clock.now.store(0, Ordering::SeqCst);
    assert_eq!(timer.measure(&mut || {})?, 1000);
    Ok(())
}
