//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//!
//! D87: one deadline for a simulator's preparation, from its device actor's first attempt through the boot attempts of
//! every preparation and the recoveries that set it up again after a failed boot, so that these retries cannot multiply.

use super::*;
use jcompat::jtime::Duration;

/// A preparation that has not ended in a ready simulator. It passes from a simulator that failed to boot to the one its
/// recovery sets up in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparationEpisode {
    /// On the run's vendor clock (`services.context.clock`).
    deadline: i64,
    /// Whether a recovery handed it to a replacement. Only a replacement's boots are held to the deadline: the first
    /// preparation's two fit in it by construction, unless something before them took long (waiting for the artifacts of
    /// a predecessor, say), and a single failed boot there keeps its second attempt as before.
    replacement: bool,
}

fn millis(duration: Duration) -> i64 {
    duration.to_millis().unwrap_or(i64::MAX)
}

impl AppleSimulatorDevice {
    fn vendor_now(&self) -> i64 {
        self.services.context.clock.now_millis()
    }
    /// One preparation with both boot attempts running to their limits: 2 × (`bootWait` + `boot` + `shutdown` +
    /// `shell`) and the pause between them.
    pub(crate) fn preparation_envelope_millis(&self) -> i64 {
        let t = &self.services.vendor.timeout_configuration;
        let attempt = millis(t.boot_wait)
            .saturating_add(millis(t.boot))
            .saturating_add(self.boot_reserve_millis());
        attempt
            .saturating_mul(2)
            .saturating_add(self.services.timeouts.boot_retry_pause_millis)
    }
    /// What a failed boot attempt needs after its wait: the shutdown, limited to `shutdown` + `shell`.
    pub fn boot_reserve_millis(&self) -> i64 {
        let t = &self.services.vendor.timeout_configuration;
        millis(t.shutdown).saturating_add(millis(t.shell))
    }
    /// The device actor's start: continues the episode this simulator took over, or opens one.
    pub(super) fn open_preparation(&self, configuration: &Configuration) -> i64 {
        let now = self.vendor_now();
        let budget =
            marathon_core::vendor::preparation_budget_millis(configuration, self.preparation_envelope_millis());
        let mut state = self.state();
        let episode = state.episode.get_or_insert(PreparationEpisode {
            deadline: now.saturating_add(budget),
            replacement: false,
        });
        episode.deadline.saturating_sub(now).max(0)
    }
    /// The preparation that has not ended in a ready simulator, if any.
    pub fn preparation_episode(&self) -> Option<PreparationEpisode> {
        self.state().episode
    }
    /// Takes over the episode of the simulator this one replaces.
    pub fn continue_preparation(&self, episode: PreparationEpisode) {
        self.state().episode = Some(PreparationEpisode {
            replacement: true,
            ..episode
        });
    }
    /// Millis left in the unfinished episode; negative once its deadline has passed.
    pub fn preparation_left_millis(&self) -> Option<i64> {
        let episode = self.state().episode?;
        Some(episode.deadline.saturating_sub(self.vendor_now()))
    }
    pub(super) fn finish_preparation(&self) {
        self.state().episode = None;
    }
    /// The left of a replacement's episode, if this simulator is one.
    fn replacement_left_millis(&self) -> Option<i64> {
        let episode = self.state().episode.filter(|e| e.replacement)?;
        Some(episode.deadline.saturating_sub(self.vendor_now()))
    }
    /// How long a boot attempt may wait for the simulator: `None` for the whole of `bootWait` (not a replacement), else
    /// what is left with the reserve for its shutdown kept. Zero or less means no time is left for one.
    pub(super) fn boot_window_millis(&self) -> Option<i64> {
        Some(
            self.replacement_left_millis()?
                .saturating_sub(self.boot_reserve_millis()),
        )
    }
    /// Whether a replacement's boot attempt after `pause_millis` would still have time to wait, with the reserve.
    pub(super) fn time_for_another_boot(&self, pause_millis: i64) -> bool {
        self.replacement_left_millis().is_none_or(|left| {
            left.saturating_sub(pause_millis)
                .saturating_sub(self.boot_reserve_millis())
                > 0
        })
    }
}
