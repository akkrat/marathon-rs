# x85 — one deadline across a device's preparation and its recovery retries (D87, U209)

A device that cannot be prepared could keep its slot in the run far longer than any one of its timeouts says, because
retries were nested and each layer counted its own attempts: the device actor's 30 attempts 10 s apart around a
preparation, the Android installer's three attempts per package inside it, and on iOS the two boot attempts of every
preparation times the three replacements a recovery sets up after a failed boot. This package gives each preparation one
deadline that every layer reads, so that the layers can no longer multiply. Approved as D87 (2026-11-18, project owner's
decision).

## The evidence, re-verified

Checked against e8cd80be (x82, which this branch is based on; the files quoted were not changed by x82 in the places
used).

- **Device actor.** `crates/marathon-core/src/execution/device/actor.rs` (`initialize`): `with_retry(30, 10_000, …)` around
  `Device::prepare`. A `VendorError::LeavePool` (iOS `NotBooted`, `NotAdmitted`, `TransferStalled`) and a cancellation
  are not retried; every other error is, 30 times.
- **Android installer.** `crates/marathon-android/src/installer.rs` (`reinstall`): three attempts per package, 1 s apart.
  An install that times out is `AndroidKind::Install` ("Timeout installing …", `transfers.rs`, `install_package`), mapped
  to `DeviceSetup`, so the actor retries the whole preparation: 30 × (3 × 60 s + 2 s) + 29 × 10 s = 5,750 s, about 96
  minutes, for one package whose install times out every time. Confirmed.
- **iOS boot.** `crates/marathon-ios/src/apple_simulator_device/boot.rs` (`boot_for_preparation`): two attempts, each a
  `bootWait` of polling, then a shutdown limited to `shutdown` + `shell`, 10 s apart. Two failures are `NotBooted`, a
  `LeavePool`: the actor does not retry it, the simulator is marked lost.
- **iOS recovery.** `crates/marathon-ios/src/device/simulator_provider/recovery.rs` (`reconnect`, `recover`): each loss of a
  simulator counts; up to three recoveries, after pauses of 0, 30 and 60 s, each re-provisions the simulator, whose new
  handle gets a new device actor and a whole new preparation. So a probe that never succeeds costs four preparations,
  about 1,570 s with a probe failing at once, 1,810 s with one timing out at `shell`, and 1,970 s with one needing KILL
  ([x42](x42-findings.md), "With another simulator alive"). Confirmed by the witness below, scaled: 25.4 s for what the
  deadline ends at 8.1 s.
- **Apple installer.** `crates/marathon-apple/src/apple_application_installer.rs` (`prepare_installation`):
  `with_retry_send(3, 1000)` around the transfer. A stalled transfer is `TransferStalled` (D54), a `LeavePool`, and is not
  retried by either layer; a transfer that progresses has no limit (D54).
- **macOS.** `crates/marathon-macos/src/device.rs` (`prepare`): no recovery replaces a Mac during its preparation, so the
  nesting is the actor's 30 attempts around the installer's three. A Mac whose worker leaves every command hanging fails
  each preparation at its first command, `shell` plus up to 20 s for KILL, so 30 × 50 + 290 = 1,790 s, about 30 minutes.

## Design

**One deadline per preparation episode.** A new `Device::preparation_time_left_millis(&Configuration) -> Option<i64>`
(`crates/marathon-core/src/vendor.rs`) opens the device's episode and says how much of it is left. The device actor calls
it once, when its preparation starts, and turns it into a deadline on its own clock; the vendor keeps the same deadline
on its clock. Passing a length rather than a time keeps the two clocks out of each other's way: in production both are the
run's timeline, in some tests they start at different instants. The default is `None`, no deadline: the Stub vendor and
test vehicles keep Kotlin's 30 attempts, so no oracle or differential recording changes (below).

**Its length** comes from existing configuration only (`preparation_budget_millis`): the greater of
`deviceInitializationTimeoutMillis` and one preparation with the retries inside it at their limits:

| Vendor | Envelope | Defaults |
|---|---|---|
| iOS | 2 × (`bootWait` + `boot` + `shutdown` + `shell`) + the 10 s pause between boot attempts | 2 × (180 + 30 + 30 + 30) + 10 = 550 s |
| Android | per package to install (the app if any, each extra app, the test app): 3 × (`shell` + `uninstall` + `pushFile` + `install` + `shell`) + 2 × 1 s; plus `shell` (`logcat -c`) and, with mock location, 2 × `shell` per bundle with an app (`appops set`, then `appops query` if it failed) | 2 × (3 × 180 + 2) + 20 = 1,104 s with one app and its test app; 1,144 s with mock location |
| macOS | 11 × `shell` (three directory commands, two in each of the installer's three attempts, the test type's probe, the test root's directory) + 2 × 1 s; the transfer is not counted, it has no limit while it progresses (D54) | 332 s |

With the defaults every envelope exceeds the actor's own schedule for a preparation that fails at once (30 attempts, 290 s),
so such a device keeps its 30 attempts. Only slow failures are cut. The Android figure is the legacy push-and-install path
in every attempt; a streaming (`abb_exec`/`cmd`) or split install takes less. A tighter figure for those would need the
device's features at the actor's start and the same choice in the installer's retry test; it is left as is (owner's
decision, review round 1).

**The rule.** The deadline only refuses to start: an attempt, retry or replacement starts only before it, and whether it
may start is asked where it starts, after whatever pause or wait precedes it (and also before the pause, so that no pause is
spent on a retry that could not start). Nothing running is stopped by it.

**What reads it.**

1. The device actor (`with_retry_until`, `crates/marathon-core/src/executor/retry.rs`): a retry only if it starts before the
   deadline, asked before the 10 s delay and after it; otherwise the last failure and `Device <serial> is not prepared
   within its preparation time of <n> ms, not retrying after <k> attempts` (WARN).
2. The Android installer (`AndroidAppInstaller::with_deadline`): a package again only if a whole attempt fits before the
   deadline, asked before the 1 s pause and after it (WARN `Not installing <package> on <serial> again: …`). The first
   attempt of each package always runs. The remote parser's installation is not part of a preparation.
3. The Apple installer (`AppleApplicationInstaller::with_retry_gate`, `with_retry_send_gated`): a failed transfer is sent
   again only before the deadline, asked before and after the pause. iOS and macOS give it their deadline; the xctest
   parser's installation has none.
4. iOS keeps the episode on the simulator handle (`apple_simulator_device/episode.rs`; `PreparationEpisode::may_start` is
   the one test):
   - The first preparation's boots are as before: both whole.
   - A recovery after a failed preparation sets the simulator up again only if the setup starts before the deadline, asked
     before its pause and after it, and only while the recovery's D44 claim is current; otherwise `Simulator <serial> has
     not got ready within its preparation time, not re-provisioning it again`, and the simulator is marked exhausted
     under that claim (`exhaust_claimed`). A recovery whose claim is stale (the simulator was removed or added back) leaves
     it to D44's paths, so a simulator added back starts afresh.
   - The episode is handed on through `carried`, with the generation of the recovery's claim, until a setup of that
     generation registers: the recovery's own, or a D33 or worker retry of it. The provider's admission takes it; a
     registration past the deadline is dropped (`its new setup is dropped`) and the simulator exhausted. A removal clears
     it, and a stale generation is discarded.
   - A replacement's first boot attempt is whole (the whole of `bootWait`), even with less left. Its second attempt is
     held: it starts only if its bound is ahead, the bound being the deadline less `shutdown` + `shell` and the 30 s a cut
     command takes to end over SSH (D30, `sshj::client::TIMEOUT_MILLIS`); every state read, readiness probe and `simctl
     boot` of it is cut at the bound (`within`), so the shutdown starts with its reserve intact.
   - A boot that fails for want of time (a held attempt, or one refused) returns `PrepareError::OutOfTime`, on which the
     preparation lets the installation beside it finish instead of cancelling it, then leaves as not booted.
   - A preparation that ends ready closes the episode, so a later loss in a batch starts afresh.
5. macOS: the actor's layer and the Apple installer's gate.

**Departure** is through the existing paths: the actor's failed preparation (`Terminate`), iOS `NotBooted`, the recovery's
give-up and the dropped setup. D48 still ends a pool that has no device sooner. Nothing new waits, so nothing can hang on
the last device of a pool.

**Not capped:** a transfer that keeps progressing (D54), and any step already running, except the reads, probes and boot
command of a replacement's held second attempt, which are cut at its bound. A rsync that runs past the deadline finishes.

## Invariants

- No retry, boot attempt, transfer retry, setup or replacement of a preparation starts after its deadline; the test is made
  where it starts.
- The first preparation runs as before, and a replacement's first boot is whole; a healthy run makes no other attempt.
- The deadline cancels nothing but the commands of a held boot attempt; a transfer is never cancelled because of it.
- A held attempt's shutdown starts with `shutdown` + `shell` left before the deadline.
- A recovery decides by the deadline only for the generation it claims.
- A device without an envelope (Stub, test vehicles) has no deadline.

## Worst cases, before and after

Scenario estimates: computed from the code with the defaults, one failure mode, the other commands answering at once and a
live sibling (so D48 does not apply); measured only on the witnesses' scaled timelines. "Before" is 0.14.0 / e8cd80be.

| Case | Before | After |
|---|---|---|
| iOS, probe fails at once | 4 preparations, about 1,570 s (26 min) | preparation (fails at 371 s), one replacement with one whole boot attempt: given up at about 552 s (9 min) |
| iOS, probe times out at `shell` (30 s) at each check | about 1,810 s (30 min) | preparation fails at 430 s, replacement's whole attempt to 640 s (11 min) |
| iOS, probe needs KILL (50 s) | about 1,970 s (33 min) | 470 s, then 700 s (12 min) |
| iOS, in general | — | the deadline plus one whole boot attempt of a replacement started just before it and its last probe: 550 + 270 + 50 = 870 s (14.5 min) |
| iOS, any of these, pool's only simulator | 9 to 11 min (D48, x42) | unchanged: D48's wait from the first departure ends it |
| Android, one package whose install times out at 60 s every time | 30 × 182 + 290 = 5,750 s (96 min) | eight preparations, the last three with one attempt: leaves at about 1,160 s (19 min) |
| macOS, every command hanging until KILL | about 1,790 s (30 min) | six preparations: about 350 s (6 min) |
| Any vendor, preparation failing at once | 30 attempts, 290 s | unchanged with default timeouts (each envelope ≥ 290 s) |

The general bound: the deadline, plus what started before it (on iOS a replacement's whole boot attempt; on Android one
attempt per package of a preparation started just before it; a transfer, which has no limit). With
`deviceInitializationTimeoutMillis` above the envelope the deadline is that instead.

## What was verified and how

Witnesses; each fails on 2fed5b75 (checked by putting 2fed5b75's sources back under the new tests) or, for the first
round's, without its fix (temporary mutation). Guards pass either way, as they should.

- Core, virtual time: `device_actor.rs` `slow_failing_preparations_stop_at_the_preparation_deadline_d87`,
  `an_attempt_that_starts_before_the_deadline_runs_to_its_end_d87`; `runtime_behavior.rs`
  `a_retry_whose_delay_ends_past_the_deadline_is_not_made_d87` (a clock whose delays overshoot: one attempt, cut after the
  delay). Guards: thirty attempts without a deadline; a transient failure retried.
- iOS, virtual time, `simulator_preparation_episode.rs`:
  `a_replacement_gets_one_whole_boot_attempt_with_less_time_left_d87` (10 s left, 20 s to boot: ready);
  `a_boot_that_runs_out_of_time_leaves_the_transfer_beside_it_to_finish_d87` (the send is not cancelled; not booted only
  after it finishes); `a_held_boot_cuts_its_final_probe_so_that_the_shutdown_keeps_its_reserve_d87` (hanging probes, the
  shutdown at 110 s of 200 s, with the fake worker ending a cut command as a real one ends;
  hangs on 2fed5b75, which the 600 s guard turns into a failure);
  `a_held_boot_whose_state_read_outlasts_its_bound_does_not_boot_d87`;
  `a_send_that_fails_after_the_deadline_is_not_sent_again_d87` (one send, three on 2fed5b75);
  `a_setup_retry_after_a_failed_boot_continues_the_preparations_deadline_d87` (same deadline after a D33 retry);
  `a_setup_retry_that_registers_past_the_deadline_is_dropped_d87`;
  `an_old_recovery_out_of_time_does_not_take_a_simulator_added_back_out_of_the_run_d87` (removed while its recovery's
  disposal is held, the deadline passing meanwhile; added back, it is set up again).
- iOS, whole run in real time, `simulator_preparation_deadline.rs` (integration check, scaled: `bootWait` 3 s, commands
  0.5 s, deadline 9.1 s, a live sibling): given up within the deadline plus one boot attempt, after one replacement whose
  boot was whole; 25.4 s and three replacements without D87. Its bounds now have a whole boot attempt of slack, and its
  decisions at least a 0.3 s recovery pause. Guards: a single failed preparation replaced; a first preparation held past
  its deadline keeps its second boot.
- Android, virtual time: `reconnect_test.rs::an_android_device_whose_installs_time_out_leaves_within_its_preparation_deadline_d87`
  (`uninstall` 6 s so that every decision has at least 7 s to spare; deadline 195 s; leaves within the deadline plus one
  attempt; 16 installs); `installer_test.rs::a_preparations_envelope_counts_mock_location_per_bundle_with_an_app_d87`
  (fails with the old flat 3 × `shell`). Guard: a single timed-out install retried.
- macOS: `batch.rs::a_macs_preparation_deadline_is_the_greater_of_the_wait_and_its_commands_d87` (332 s).

**Oracle and differential recordings.** None changes, so no comparator allowance is needed: the device-actor oracle
(`fixtures/oracle/device-actor`, including `thirty failing preparations`) runs the Stub vehicle, which has no deadline; the
fake-adb Kotlin comparisons (`fixtures/fake-adb/android-reconnect`) fail at most three preparations, quickly, far inside
any deadline; the CLI differential suites ran unchanged.

**Golden diffs:** none. Result files, the Marathonfile, the CLI and worker commands are unchanged except for the commands a
device no longer gets (later attempts, boots, transfers and replacements) and the cut commands of a held boot attempt.

## Not verified

- No run on a real simulator, emulator or Mac.
- The macOS deadline is exercised through the core actor's tests, the shared Apple installer gate (iOS witness) and a
  pinned budget, not a whole macOS run.
- The Android installer's re-check after its pause has no witness of its own: in virtual time the pause is exact, so the
  check before it already decides; the core witness covers the same overshoot with the actor's delay.
- The worst-case figures are scenario estimates, not measurements with the defaults.

## Owner decisions

- **Progressing transfers stay uncapped (D54)** (decided in review round 1): the deadline never cancels a transfer.
- **Android keeps the legacy push-and-install envelope**, about 18 minutes with the defaults and two packages (decided in
  review round 1).

## Review round 1

1. *A failed replacement setup reset the episode.* The carried episode now stays, with the recovery's claim generation,
   until a setup of that generation registers (D33 or worker retry included); past the deadline the registration is
   dropped.
2. *A transient outage could cost a healthy replacement, and a shortened wait could cancel a transfer.* A replacement's
   first boot is whole; only its second is held; a boot out of time leaves the installation to finish.
3. *An old recovery could exhaust a re-added simulator.* The deadline applies only under a current claim, and exhaustion
   is written under it (`exhaust_claimed`).
4. *The reserve could be spent before the shutdown.* A held attempt cuts every read, probe and `simctl boot` at its bound,
   which keeps `shutdown` + `shell` and D30's 30 s before the deadline.
5. *Checks before waits.* One test, made where each attempt starts: after the actor's delay, the installers' pauses, the
   recovery's pause and the provisioning permit (at admission).
6. *Formulas.* Android counts mock-location commands per app bundle; macOS counts the test root's directory; the figures are
   labelled scenario estimates.
7. *Wall-clock iOS witness.* The exact behaviour is now witnessed in virtual time with a gated slow final probe; the
   real-time run stays as an integration check with a whole boot attempt of slack.

## Incidental bugs

None found.
