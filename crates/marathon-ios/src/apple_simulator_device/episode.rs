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
    /// Boot attempts started in it. The first is never shortened, so that a simulator whose preparation waited long
    /// before it booted (for the artifacts of its predecessor, say) still gets the whole of `bootWait` once.
    boots: u32,
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
            boots: 0,
        });
        episode.deadline.saturating_sub(now).max(0)
    }
    /// The preparation that has not ended in a ready simulator, if any.
    pub fn preparation_episode(&self) -> Option<PreparationEpisode> {
        self.state().episode
    }
    /// Takes over the episode of the simulator this one replaces.
    pub fn continue_preparation(&self, episode: PreparationEpisode) {
        self.state().episode = Some(episode);
    }
    /// Millis left in the unfinished episode; negative once its deadline has passed.
    pub fn preparation_left_millis(&self) -> Option<i64> {
        let episode = self.state().episode?;
        Some(episode.deadline.saturating_sub(self.vendor_now()))
    }
    pub(super) fn finish_preparation(&self) {
        self.state().episode = None;
    }
    /// Starts a boot attempt: how long it may wait for the simulator, or `None` for the whole of `bootWait` (no
    /// episode, or its first boot). A window of zero or less means no time is left for one.
    pub(super) fn boot_window_millis(&self) -> Option<i64> {
        let now = self.vendor_now();
        let reserve = self.boot_reserve_millis();
        let mut state = self.state();
        let episode = state.episode.as_mut()?;
        episode.boots = episode.boots.saturating_add(1);
        if episode.boots == 1 {
            return None;
        }
        Some(episode.deadline.saturating_sub(now).saturating_sub(reserve))
    }
    /// Whether a boot attempt after `pause_millis` would still have time to wait, with the reserve for its shutdown.
    pub(super) fn time_for_another_boot(&self, pause_millis: i64) -> bool {
        self.preparation_left_millis().is_none_or(|left| {
            left.saturating_sub(pause_millis)
                .saturating_sub(self.boot_reserve_millis())
                > 0
        })
    }
}
