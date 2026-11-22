//! x110: how long the end of a run took, one INFO line per step on the timing target, in the same words on every
//! vendor. Diagnostics only (§3.12.5): none of it enters the pinned progress or summary text.

/// A duration as the log shows it: whole milliseconds under a second, else seconds with one decimal.
pub fn duration_text(millis: i64) -> String {
    let millis = millis.max(0);
    if millis < 1000 {
        return format!("{millis} ms");
    }
    let tenths = millis.saturating_add(50) / 100;
    format!("{}.{} s", tenths / 10, tenths % 10)
}

/// `<count> <noun>`, the noun plural for any count but one.
fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The platform a device release line names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Android,
    Ios,
    Macos,
}
impl Platform {
    pub const ALL: [Platform; 3] = [Platform::Android, Platform::Ios, Platform::Macos];
    pub fn name(self) -> &'static str {
        match self {
            Platform::Android => "Android",
            Platform::Ios => "iOS",
            Platform::Macos => "macOS",
        }
    }
}

/// The slowest of several timed parts, by its own name (a device's serial, a reporter's name).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slowest {
    count: usize,
    slowest: Option<(String, i64)>,
    /// A part that had not finished when the step stopped waiting: the finished slowest is then not the slowest.
    unfinished: bool,
}
impl Slowest {
    pub fn add(&mut self, name: impl Into<String>, millis: i64) {
        self.count += 1;
        // The first of equal durations is kept, so the order of the parts decides a tie.
        if self.slowest.as_ref().is_none_or(|(_, slowest)| millis > *slowest) {
            self.slowest = Some((name.into(), millis));
        }
    }
    /// A part the step stopped waiting for: counted, and no part is named slowest.
    pub fn add_unfinished(&mut self) {
        self.count += 1;
        self.unfinished = true;
    }
    /// The state a line shows: `count` parts and the one it names slowest, if any (none named means a part was
    /// unfinished, or there were none; none of no parts is ever named). For reading a line back
    /// (`marathon_test_support::run_end`).
    pub fn shown(count: usize, slowest: Option<(String, i64)>) -> Self {
        let slowest = slowest.filter(|_| count > 0);
        Self {
            count,
            unfinished: slowest.is_none() && count > 0,
            slowest,
        }
    }
    pub fn count(&self) -> usize {
        self.count
    }
    /// `; slowest <name>, <duration>` when at least one part finished and none was left unfinished.
    fn suffix(&self) -> String {
        match &self.slowest {
            Some((name, millis)) if !self.unfinished => format!("; slowest {name}, {}", duration_text(*millis)),
            _ => String::new(),
        }
    }
}

/// The provider's wait at its termination for the artifact work still queued or running (iOS pulls and cleanups).
pub fn artifact_finalization(millis: i64, items: usize) -> String {
    format!(
        "Artifact finalization at run end took {} ({} queued or running)",
        duration_text(millis),
        counted(items, "item")
    )
}

/// The disposal and release of every device at the provider's termination. `stopping` is the part of `millis` the
/// provider spent outside the devices' own release, stopping its monitoring, recovery and event publishing (iOS joins
/// them before and after the devices' release); a provider that stops them beside the devices passes `None`.
pub fn device_release(platform: Platform, millis: i64, devices: &Slowest, stopping: Option<i64>) -> String {
    let stopping = stopping.map_or(String::new(), |millis| {
        format!("; stopping monitoring, recovery and events {}", duration_text(millis))
    });
    format!(
        "Device release at run end took {} ({}, {}{}{stopping})",
        duration_text(millis),
        platform.name(),
        counted(devices.count(), "device"),
        devices.suffix()
    )
}

/// All reporters' generation at the run's end, in their configured order.
pub fn reports(millis: i64, reporters: &Slowest) -> String {
    format!(
        "Reports at run end took {} ({}{})",
        duration_text(millis),
        counted(reporters.count(), "reporter"),
        reporters.suffix()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_whole_milliseconds_under_a_second_and_tenths_of_a_second_above() {
        assert_eq!(duration_text(-5), "0 ms");
        assert_eq!(duration_text(0), "0 ms");
        assert_eq!(duration_text(999), "999 ms");
        assert_eq!(duration_text(1000), "1.0 s");
        assert_eq!(duration_text(1049), "1.0 s");
        assert_eq!(duration_text(1050), "1.1 s");
        assert_eq!(duration_text(2_949), "2.9 s");
        assert_eq!(duration_text(61_000), "61.0 s");
    }

    #[test]
    fn the_slowest_part_is_named_whenever_one_finished_and_none_was_left() {
        let mut one = Slowest::default();
        one.add("A", 1200);
        assert_eq!(
            device_release(Platform::Ios, 1300, &one, Some(100)),
            "Device release at run end took 1.3 s (iOS, 1 device; slowest A, 1.2 s; stopping monitoring, recovery and events 100 ms)"
        );
        let mut several = Slowest::default();
        several.add("A", 400);
        several.add("B", 2900);
        several.add("C", 2900);
        assert_eq!(
            device_release(Platform::Android, 2950, &several, None),
            "Device release at run end took 3.0 s (Android, 3 devices; slowest B, 2.9 s)"
        );
        let mut stopped = several.clone();
        stopped.add_unfinished();
        assert_eq!(
            device_release(Platform::Android, 65_000, &stopped, None),
            "Device release at run end took 65.0 s (Android, 4 devices)"
        );
        assert_eq!(
            device_release(Platform::Macos, 0, &Slowest::default(), None),
            "Device release at run end took 0 ms (macOS, 0 devices)"
        );
        assert_eq!(
            reports(312, &several),
            "Reports at run end took 312 ms (3 reporters; slowest B, 2.9 s)"
        );
        assert_eq!(
            reports(5, &one),
            "Reports at run end took 5 ms (1 reporter; slowest A, 1.2 s)"
        );
        assert_eq!(
            artifact_finalization(1500, 1),
            "Artifact finalization at run end took 1.5 s (1 item queued or running)"
        );
        assert_eq!(
            artifact_finalization(3, 0),
            "Artifact finalization at run end took 3 ms (0 items queued or running)"
        );
    }

    #[test]
    fn a_shown_state_words_its_line_as_the_measured_one() {
        let mut several = Slowest::default();
        several.add("A", 400);
        several.add("B", 2900);
        assert_eq!(Slowest::shown(2, Some(("B".into(), 2900))), several);
        let mut stopped = Slowest::default();
        stopped.add_unfinished();
        assert_eq!(reports(1, &Slowest::shown(1, None)), reports(1, &stopped));
        assert_eq!(Slowest::shown(0, None), Slowest::default());
        assert_eq!(Slowest::shown(0, Some(("A".into(), 1))), Slowest::default());
    }
}
