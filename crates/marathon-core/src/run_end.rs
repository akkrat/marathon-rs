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
    pub fn count(&self) -> usize {
        self.count
    }
    /// `; slowest <name>, <duration>` when there was more than one part.
    fn suffix(&self) -> String {
        match &self.slowest {
            Some((name, millis)) if self.count > 1 && !self.unfinished => {
                format!("; slowest {name}, {}", duration_text(*millis))
            }
            _ => String::new(),
        }
    }
}

/// The provider's wait at its termination for the artifact work still queued or running (iOS pulls and cleanups).
pub fn artifact_finalization(millis: i64, items: usize) -> String {
    let noun = if items == 1 { "item" } else { "items" };
    format!(
        "Artifact finalization at run end took {} ({items} {noun} queued or running)",
        duration_text(millis)
    )
}

/// The disposal and release of every device at the provider's termination.
pub fn device_release(vendor: &str, millis: i64, devices: &Slowest) -> String {
    let count = devices.count();
    let noun = if count == 1 { "device" } else { "devices" };
    format!(
        "Device release at run end took {} ({vendor}, {count} {noun}{})",
        duration_text(millis),
        devices.suffix()
    )
}

/// All reporters' generation at the run's end, in their configured order.
pub fn reports(millis: i64, reporters: &Slowest) -> String {
    let count = reporters.count();
    let noun = if count == 1 { "reporter" } else { "reporters" };
    format!(
        "Reports at run end took {} ({count} {noun}{})",
        duration_text(millis),
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
    fn the_slowest_part_is_named_only_among_several() {
        let mut one = Slowest::default();
        one.add("A", 1200);
        assert_eq!(
            device_release("iOS", 1300, &one),
            "Device release at run end took 1.3 s (iOS, 1 device)"
        );
        let mut several = Slowest::default();
        several.add("A", 400);
        several.add("B", 2900);
        several.add("C", 2900);
        assert_eq!(
            device_release("Android", 2950, &several),
            "Device release at run end took 3.0 s (Android, 3 devices; slowest B, 2.9 s)"
        );
        let mut stopped = several.clone();
        stopped.add_unfinished();
        assert_eq!(
            device_release("Android", 65_000, &stopped),
            "Device release at run end took 65.0 s (Android, 4 devices)"
        );
        assert_eq!(
            device_release("macOS", 0, &Slowest::default()),
            "Device release at run end took 0 ms (macOS, 0 devices)"
        );
        assert_eq!(
            reports(312, &several),
            "Reports at run end took 312 ms (3 reporters; slowest B, 2.9 s)"
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
}
