//! Port of core/src/main/kotlin/com/malinskiy/marathon/analytics/external/influx/InfluxDbTracker.kt
//! and external/influx2/InfluxDb2Tracker.kt (Point encoding).
use crate::http::text;
use jcompat::{
    file_manager::escape as escape_serial,
    jnum::influx_duration,
    jtime::{influx_epoch_millis, instant_to_epoch_milli},
};
use marathon_core::{error::OperationResult, executor::shared::failure, model::TestStatus, report::TestEvent};

pub(crate) fn millis(event: &TestEvent) -> OperationResult<i64> {
    instant_to_epoch_milli(event.instant)
        .ok_or_else(|| failure("java.lang.ArithmeticException", "long overflow").into())
}
fn escape(value: &str, v2: bool) -> String {
    let mut out = String::new();
    for c in value.chars() {
        match c {
            ' ' | ',' | '=' => {
                out.push('\\');
                out.push(c);
            }
            '\n' if v2 => out.push_str("\\n"),
            '\r' if v2 => out.push_str("\\r"),
            '\t' if v2 => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}
pub(crate) fn line(event: &TestEvent, v2: bool) -> OperationResult<String> {
    let r = &event.test_result;
    let t = &r.test;
    let serial = escape_serial(&event.device.serial_number);
    let mut out = String::from("tests");
    // Both pinned Point classes store tags and fields in TreeMap order.
    for (key, value) in [
        ("class", &t.clazz),
        ("deviceSerial", &serial),
        ("method", &t.method),
        ("package", &t.pkg),
        ("testname", &t.to_safe_test_name()),
    ] {
        let value = text(value)?;
        if !value.is_empty() {
            out.push_str(&format!(",{key}={}", escape(&value, v2)));
        }
    }
    let duration = r.end_time.wrapping_sub(r.start_time);
    out.push_str(&format!(
        " duration={},ignored={}",
        if v2 {
            influx_duration(duration)
        } else {
            format!("{duration}i")
        },
        if r.is_ignored() { "1.0" } else { "0.0" }
    ));
    if !v2 || r.status != TestStatus::Incomplete {
        out.push_str(if r.status == TestStatus::Failure {
            ",success=0.0"
        } else {
            ",success=1.0"
        });
    }
    // v2's BigInteger precision conversion truncates toward zero and has no Long bound.
    let time = if v2 {
        influx_epoch_millis(event.instant)
    } else {
        i128::from(millis(event)?.saturating_mul(1_000_000))
    };
    out.push_str(&format!(" {time}"));
    Ok(out)
}
