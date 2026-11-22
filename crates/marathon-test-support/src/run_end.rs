//! x110: the run-end timing lines (`marathon_core::run_end`) are diagnostics of this port only (§3.12.5), so a
//! comparison with a JVM recording leaves out exactly those lines: an INFO record on the timing target whose whole
//! message has one of their three shapes. Anything else on that target, at another level or on another target, stays.

use crate::console::Line;
use marathon_core::run_end::{Platform, Slowest, artifact_finalization, device_release, reports};
use marathon_logging::{Level, TIMINGS};

/// The x110 allowance for one record of the console.
pub fn is_x110_run_end_timing(line: &Line) -> bool {
    matches!(line, Line::Log { logger, level, message, cause: None }
        if is_x110_run_end_record(logger, *level, message))
}

/// The same allowance for a record given by its parts.
pub fn is_x110_run_end_record(logger: &str, level: Level, message: &str) -> bool {
    logger == TIMINGS && level == Level::INFO && is_x110_message(message)
}

/// A message is one of the three lines exactly when the formatter, given the values read from it, writes it again: the
/// reading below may be loose, the comparison holds the grammar to the formatter's (durations, plurals, which parts
/// are named), so the two cannot drift.
fn is_x110_message(message: &str) -> bool {
    rewritten(message).is_some_and(|written| written == message)
}

fn rewritten(message: &str) -> Option<String> {
    if let Some(rest) = message.strip_prefix("Reports at run end took ") {
        let (took, inner) = parenthesised(rest)?;
        let mut parts = inner.split("; ");
        let count = count(parts.next()?)?;
        let slowest = match parts.next() {
            Some(part) => Some(slowest(part)?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(reports(took, &Slowest::shown(count, slowest)))
    } else if let Some(rest) = message.strip_prefix("Device release at run end took ") {
        let (took, inner) = parenthesised(rest)?;
        let mut parts = inner.split("; ").peekable();
        let (platform, head) = parts.next()?.split_once(", ")?;
        let platform = *Platform::ALL.iter().find(|p| p.name() == platform)?;
        let count = count(head)?;
        let slowest = match parts.next_if(|part| part.starts_with("slowest ")) {
            Some(part) => Some(slowest(part)?),
            None => None,
        };
        let stopping = match parts.next() {
            Some(part) => Some(duration(
                part.strip_prefix("stopping monitoring, recovery and events ")?,
            )?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(device_release(
            platform,
            took,
            &Slowest::shown(count, slowest),
            stopping,
        ))
    } else if let Some(rest) = message.strip_prefix("Artifact finalization at run end took ") {
        let (took, inner) = parenthesised(rest)?;
        Some(artifact_finalization(
            took,
            count(inner.strip_suffix(" queued or running")?)?,
        ))
    } else {
        None
    }
}

/// `<duration> (<inner>)`.
fn parenthesised(text: &str) -> Option<(i64, &str)> {
    let (took, inner) = text.split_once(" (")?;
    Some((duration(took)?, inner.strip_suffix(')')?))
}

fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `<n> ms` or `<s>.<tenth> s`, as milliseconds.
fn duration(text: &str) -> Option<i64> {
    if let Some(ms) = text.strip_suffix(" ms") {
        return number(ms);
    }
    let (whole, tenth) = text.strip_suffix(" s")?.split_once('.')?;
    let (whole, tenth): (i64, i64) = (number(whole)?, number(tenth)?);
    (tenth < 10)
        .then(|| whole.checked_mul(1000)?.checked_add(tenth * 100))
        .flatten()
}

/// `<n> <noun>`: the number only; the noun is the formatter's to write.
fn count(text: &str) -> Option<usize> {
    number(text.split_once(' ')?.0)
}

/// `slowest <name>, <duration>`.
fn slowest(text: &str) -> Option<(String, i64)> {
    let (name, took) = text.strip_prefix("slowest ")?.rsplit_once(", ")?;
    Some((name.to_owned(), duration(took)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(message: &str) -> Line {
        Line::Log {
            logger: TIMINGS.into(),
            level: Level::INFO,
            message: message.into(),
            cause: None,
        }
    }

    #[test]
    fn the_x110_lines_the_port_writes_are_left_out() {
        let mut several = Slowest::default();
        several.add("AllureReporter", 1200);
        several.add("127.0.0.1:5037:emulator-5554", 30);
        let mut one = Slowest::default();
        one.add("x", 1);
        let mut stopped = several.clone();
        stopped.add_unfinished();
        let mut left = Slowest::default();
        left.add_unfinished();
        for message in [
            reports(5, &several),
            reports(0, &Slowest::default()),
            reports(1_234_567, &one),
            device_release(Platform::Android, 2950, &several, None),
            device_release(Platform::Android, 65_000, &stopped, None),
            device_release(Platform::Android, 65_000, &left, None),
            device_release(Platform::Ios, 0, &one, Some(0)),
            device_release(Platform::Ios, 4100, &several, Some(1200)),
            device_release(Platform::Ios, 10, &Slowest::default(), Some(10)),
            device_release(Platform::Macos, 300, &one, None),
            device_release(Platform::Macos, 0, &Slowest::default(), None),
            artifact_finalization(1500, 1),
            artifact_finalization(3, 0),
            artifact_finalization(999, 2),
        ] {
            assert!(is_x110_run_end_timing(&info(&message)), "{message}");
        }
    }

    #[test]
    fn other_records_stay_negative_x110() {
        // Another timing line, the right text on another target or at another level, and near misses of each shape.
        assert!(!is_x110_run_end_timing(&info(
            "Device provider initialized in 0 ms (+0 ms since start)"
        )));
        let line = "Reports at run end took 5 ms (10 reporters; slowest TimelineReporter, 3 ms)";
        assert!(is_x110_run_end_timing(&info(line)));
        assert!(!is_x110_run_end_record("marathon_core::marathon", Level::INFO, line));
        assert!(!is_x110_run_end_record(TIMINGS, Level::WARN, line));
        assert!(!is_x110_run_end_timing(&Line::Raw(line.into())));
        assert!(!is_x110_run_end_timing(&Line::Log {
            logger: TIMINGS.into(),
            level: Level::INFO,
            message: line.into(),
            cause: Some(marathon_core::executor::shared::failure(
                marathon_error::FailureKind::IllegalState,
                "x"
            )),
        }));
        for near in [
            "Reports at run end took 5 ms (10 reporters; slowest TimelineReporter, 3 ms) and more",
            "Reports at run end took 5ms (10 reporters)",
            "Reports at run end took 1.25 s (10 reporters)",
            "Reports at run end took 5 ms (10 tests)",
            "Reports at run end took 5 ms (ten reporters)",
            "Reports at run end took 5 ms",
            // Durations the formatter never writes: a second or more in ms, under a second in s, padding, signs.
            "Reports at run end took 1000 ms (10 reporters)",
            "Reports at run end took 0.9 s (10 reporters)",
            "Reports at run end took 01.5 s (10 reporters)",
            "Reports at run end took 05 ms (10 reporters)",
            "Reports at run end took -5 ms (10 reporters)",
            "Reports at run end took 1.5 s (10 reporters; slowest A, 1000 ms)",
            "Reports at run end took 1.5 s (10 reporters; slowest A, 0.5 s)",
            // Plurals: the noun is singular for one and only for one.
            "Reports at run end took 5 ms (1 reporters; slowest A, 1 ms)",
            "Reports at run end took 5 ms (2 reporter; slowest A, 1 ms)",
            "Reports at run end took 5 ms (0 reporter)",
            "Device release at run end took 5 ms (iOS, 1 devices; slowest A, 1 ms; stopping monitoring, recovery and events 0 ms)",
            "Device release at run end took 5 ms (Android, 2 device; slowest A, 1 ms)",
            "Artifact finalization at run end took 5 ms (1 items queued or running)",
            "Artifact finalization at run end took 5 ms (2 item queued or running)",
            // A slowest part is named whenever one finished and none was left; none is named among no parts.
            "Reports at run end took 5 ms (0 reporters; slowest A, 1 ms)",
            "Device release at run end took 5 ms (macOS, 0 devices; slowest A, 1 ms)",
            // Parts out of order, repeated or foreign.
            "Device release at run end took 5 ms (iOS, 1 device; stopping monitoring, recovery and events 0 ms; slowest A, 1 ms)",
            "Device release at run end took 5 ms (iOS, 2 devices; slowest A, 1 ms; slowest B, 1 ms)",
            "Device release at run end took 5 ms (iOS, 1 device; slowest A, 1 ms; stopping tasks 0 ms)",
            "Reports at run end took 5 ms (1 reporter; slowest A, 1 ms; stopping monitoring, recovery and events 0 ms)",
            "Device release at run end took 5 ms (Linux, 1 device)",
            "Device release at run end took 5 ms (ios, 1 device)",
            "Device release at run end took 5 ms (iOS)",
            "Artifact finalization at run end took 5 ms (2 items)",
            "Artifact finalization at run end took 5 ms (2 bundles queued or running)",
            "Scheduling 3 tests (+0 ms since start)",
        ] {
            assert!(!is_x110_run_end_timing(&info(near)), "{near}");
        }
    }
}
