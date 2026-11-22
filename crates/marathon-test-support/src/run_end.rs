//! x110: the run-end timing lines (`marathon_core::run_end`) are diagnostics of this port only (§3.12.5), so a
//! comparison with a JVM recording leaves out exactly those lines: an INFO record on the timing target whose whole
//! message has one of their three shapes. Anything else on that target, at another level or on another target, stays.

use crate::console::Line;
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

fn is_x110_message(message: &str) -> bool {
    let (detail, rest) = if let Some(rest) = message.strip_prefix("Reports at run end took ") {
        (reporters as fn(&str) -> bool, rest)
    } else if let Some(rest) = message.strip_prefix("Device release at run end took ") {
        (devices as fn(&str) -> bool, rest)
    } else if let Some(rest) = message.strip_prefix("Artifact finalization at run end took ") {
        (items as fn(&str) -> bool, rest)
    } else {
        return false;
    };
    let Some((took, inner)) = rest.split_once(" (") else {
        return false;
    };
    let Some(inner) = inner.strip_suffix(')') else {
        return false;
    };
    duration(took) && detail(inner)
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `123 ms` or `4.5 s`.
fn duration(text: &str) -> bool {
    if let Some(ms) = text.strip_suffix(" ms") {
        return digits(ms);
    }
    let Some((whole, tenth)) = text.strip_suffix(" s").and_then(|s| s.split_once('.')) else {
        return false;
    };
    digits(whole) && tenth.len() == 1 && digits(tenth)
}

/// `<n> <noun>` with the noun singular for one.
fn counted(text: &str, noun: &str) -> bool {
    let Some((count, word)) = text.split_once(' ') else {
        return false;
    };
    digits(count) && (word == noun || word.strip_suffix('s') == Some(noun))
}

/// An optional `; slowest <name>, <duration>` after `head`.
fn with_slowest(text: &str, head: impl Fn(&str) -> bool) -> bool {
    match text.split_once("; slowest ") {
        None => head(text),
        Some((first, slowest)) => {
            head(first)
                && slowest
                    .rsplit_once(", ")
                    .is_some_and(|(name, took)| !name.is_empty() && !name.contains(' ') && duration(took))
        }
    }
}

fn reporters(text: &str) -> bool {
    with_slowest(text, |head| counted(head, "reporter"))
}

fn devices(text: &str) -> bool {
    with_slowest(text, |head| {
        head.split_once(", ")
            .is_some_and(|(vendor, count)| matches!(vendor, "Android" | "iOS" | "macOS") && counted(count, "device"))
    })
}

fn items(text: &str) -> bool {
    text.strip_suffix(" queued or running")
        .is_some_and(|count| counted(count, "item"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use marathon_core::run_end::{Slowest, artifact_finalization, device_release, reports};

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
        for message in [
            reports(5, &several),
            reports(0, &Slowest::default()),
            reports(1_234_567, &one),
            device_release("Android", 2950, &several),
            device_release("iOS", 0, &one),
            device_release("macOS", 300, &Slowest::default()),
            artifact_finalization(1500, 1),
            artifact_finalization(3, 0),
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
            "Reports at run end took 5 ms (10 reporters; slowest A B, 3 ms)",
            "Reports at run end took 5 ms",
            "Device release at run end took 5 ms (Linux, 1 device)",
            "Device release at run end took 5 ms (iOS)",
            "Artifact finalization at run end took 5 ms (2 items)",
            "Artifact finalization at run end took 5 ms (2 bundles queued or running)",
            "Scheduling 3 tests (+0 ms since start)",
        ] {
            assert!(!is_x110_run_end_timing(&info(near)), "{near}");
        }
    }
}
