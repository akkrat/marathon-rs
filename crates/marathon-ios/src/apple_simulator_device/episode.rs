//! Port of vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/AppleSimulatorDevice.kt
//!
//! D87: one deadline for a simulator's preparation, from its device actor's first attempt through the boot attempts of
//! every preparation and the recoveries (and their setup retries) that set it up again after a failed boot, so that
//! these retries cannot multiply. The deadline only refuses to start work: nothing in progress is stopped or cut by it, a
//! transfer least of all (D54). A replacement's second boot attempt starts each of its commands only if, run to its own
//! limit, it ends where the shutdown after it still fits.

use super::*;
use jcompat::jtime::Duration;

/// A preparation that has not ended in a ready simulator. It passes from a simulator that failed to boot to the setups
/// its recovery makes in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparationEpisode {
    /// On the run's vendor clock (`services.context.clock`).
    deadline: i64,
    /// Whether a recovery handed it to a replacement. The first preparation's boots are as they were; a replacement's
    /// first boot is a whole one too, and only its second is held to the deadline.
    replacement: bool,
}
impl PreparationEpisode {
    /// D87's one test: whether a new attempt, retry or replacement may start at `now`, on the vendor clock.
    pub fn may_start(&self, now: i64) -> bool {
        now < self.deadline
    }
    /// When it ends, on the vendor clock.
    pub fn deadline(&self) -> i64 {
        self.deadline
    }
}

fn millis(duration: Duration) -> i64 {
    duration.to_millis().unwrap_or(i64::MAX)
}

impl AppleSimulatorDevice {
    pub(super) fn vendor_now(&self) -> i64 {
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
    pub(super) fn finish_preparation(&self) {
        self.state().episode = None;
    }
    /// Whether a new attempt or retry of this simulator's preparation may start `after` ms from now. Without an
    /// unfinished episode, always.
    pub fn may_start_attempt(&self, after: i64) -> bool {
        let episode = self.state().episode;
        episode.is_none_or(|e| e.may_start(self.vendor_now().saturating_add(after)))
    }
    /// Where a replacement's held boot attempt must have ended, its commands run to their worst cases, so that the shutdown
    /// after it, its own commands at their worst too, still ends by the deadline; `None` for a simulator that is not a
    /// replacement, whose boots are whole.
    pub(super) fn held_boot_end(&self) -> Option<i64> {
        let episode = self.state().episode.filter(|e| e.replacement)?;
        Some(episode.deadline.saturating_sub(self.shutdown_worst_case_millis()))
    }
    /// D87: the most the shutdown after a failed boot can take: its descriptor read (`shell`) and `simctl shutdown`
    /// (`shutdown`), each at its worst, from asking for its SSH channel to D30's escalation. Its own limit, `shutdown` +
    /// `shell`, ends the wait for them, but not the commands, which end through their own limits.
    fn shutdown_worst_case_millis(&self) -> i64 {
        let t = &self.services.vendor.timeout_configuration;
        let worst = |d: Duration| marathon_apple::cmd::remote::ssh::sshj::client::command_worst_case_millis(millis(d));
        worst(t.shell).saturating_add(worst(t.shutdown))
    }
}
