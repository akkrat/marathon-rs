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
| macOS | `shell` for each command with a limit that one preparation can send (`MacosDevice::preparation_commands`, listed under "Review round 3" and "Review round 4", checked against whole preparations by `tests/preparation_commands.rs`): 27 with the test type left to the probe, 15 for hosted XCTest, 26 for XCUITest, with permissions to grant the SIP check and 2 per permission, and one cache check per extra application; + 2 × 1 s. Transfers are not counted, they have no limit while they progress (D54), nor are code coverage's static-library directories, which only the built bundle names | 812 s with the test type left to the probe; 452 s hosted XCTest |

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
     held to a bound, the deadline less `shutdown` + `shell`: it starts each state read (the descriptor read and the
     readiness probe) and `simctl boot` only if each command, at its worst (`command_worst_case_millis`: the SSH channel
     and the exec's acknowledgment, 30 s each, its own limit, D30's 30 s), ends by the bound. Nothing of the attempt is cut: a
     command that started ends through its own timeout, so the shutdown decision reads the simulator's real state, and
     the shutdown, under its own `shutdown` + `shell` limit, ends by the deadline.
   - A boot that fails for want of time (a held attempt, or one refused) returns `PrepareError::OutOfTime`, on which the
     preparation lets the installation beside it finish instead of cancelling it, then leaves as not booted. A transfer
     that stalled (U163) is reported instead of either kind of boot failure, so the simulator is retired.
   - A simulator lost by a failed preparation (its episode still open) is a loss for the scan even if its worker dropped
     since, so its recovery counts it and hands the episode on.
   - A setup of a simulator carrying an expired episode does not start: the provisioner asks the recovery policy
     (`ProviderRecovery::may_set_up`) once the provisioning permit is held, before it connects.
   - A preparation that ends ready closes the episode, so a later loss in a batch starts afresh.
5. macOS: the actor's layer and the Apple installer's gate.

**Departure** is through the existing paths: the actor's failed preparation (`Terminate`), iOS `NotBooted`, the recovery's
give-up and the dropped setup. D48 still ends a pool that has no device sooner. Nothing new waits, so nothing can hang on
the last device of a pool.

**Not capped or cut:** a transfer that keeps progressing (D54), and any step already running. A rsync that runs past the
deadline finishes.

## Invariants

- No retry, boot attempt, transfer retry, setup or replacement of a preparation starts after its deadline; the test is made
  where it starts.
- The first preparation runs as before, and a replacement's first boot is whole; a healthy run makes no other attempt.
- The deadline cancels and cuts nothing; a transfer is never cancelled because of it.
- A held attempt's last command ends, run to its limit, by the deadline less `shutdown` + `shell`, so its shutdown ends by
  the deadline and decides on the simulator's real state.
- A stalled transfer is what a preparation reports, whatever its boot did.
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
| macOS, every command hanging until KILL | about 1,790 s (30 min) | fourteen preparations: about 830 s (14 min) |
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
  after it finishes); round 2's `a_held_boot_never_cuts_a_command_and_starts_none_that_could_outlast_its_bound_d87`
  (25 s probes, no command dropped, the last read refused at 330 s of a 440 s bound) and
  `a_held_boot_starts_no_command_that_could_outlast_its_bound_d87` (no boot when a read could not end by the bound),
  which replace round 1's cut-probe and slow-read tests; `a_stalled_transfer_wins_over_a_boot_out_of_time_beside_it_d87`;
  `a_worker_dropping_after_a_failed_boot_does_not_renew_the_preparations_deadline_d87`;
  `a_send_that_fails_after_the_deadline_is_not_sent_again_d87` (one send, three on 2fed5b75);
  `a_setup_retry_after_a_failed_boot_continues_the_preparations_deadline_d87` (same deadline after a D33 retry);
  `a_setup_retry_that_would_start_past_the_deadline_sets_nothing_up_d87` (no `getenv HOME` after the recovery's);
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
- macOS: `preparation_commands.rs` (each variant's longest path on the fake worker sends exactly what its budget counts:
  25, 24, 15 and 17 commands; logic tests and code coverage no more), and
  `batch.rs::a_macs_preparation_deadline_is_the_greater_of_the_wait_and_its_commands_d87` (812, 452 and 932 s; round 4's
  figures, with the cache checks).

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

## Review round 2

Orchestrator's decision: the deadline never cancels or cuts anything already running; it only refuses to start.

1. *A held attempt's cut left SSH commands running.* Cutting is gone (`within` removed). A held attempt starts each state
   read and `simctl boot` only if, run to its limit and D30's escalation, it ends by the deadline less `shutdown` +
   `shell`; the shutdown decision therefore always reads real state. The worst case is unchanged in kind: a held
   attempt, with its shutdown, ends by the deadline; the whole first attempt of a replacement started just before the
   deadline remains the only overrun (deadline + one boot attempt). Witnesses: no command dropped, the last read refused
   at its bound; no boot when a read could not end by it.
2. *Expired setup still reached the worker.* `SimulatorProvisioner::with_setup_gate` asks `ProviderRecovery::may_set_up`
   after the provisioning permit and before connecting; a refused setup is dropped and the simulator exhausted under the
   claim. The D33 witness now asserts no `getenv HOME` after the recovery's.
3. *A worker drop renewed the episode.* A lost simulator with an open episode is classified as a failed preparation
   even when disconnected (read before the provider's lock). Witness: NotBooted, worker drop, reconnect; the
   replacement continues the deadline.
4. *A held attempt swallowed a stalled transfer.* `terminal_transfer` makes a `TransferStalled` from the installation win
   over `OutOfTime` and over an ordinary boot failure whose sibling was cancelled. Witness: the simulator is retired with
   `TransferStalled`, not lost.
5. *macOS formula.* Audited every command on the preparation path; each runs under `shell`:
   - `prepare_inner`: remove, create and create-shared directory (3).
   - Apple installer, per transfer attempt: create and create-shared directory (2 × 3).
   - Test type probe (`nm`), unless the bundle configures the test type (1).
   - Test root factory `generate`: the test root directory (1).
   - Hosted XCTest (`hosted`): the app's `chmod` in `application`, the `PlugIns` directory (2). With
     `skipCopyingDerivedData` the pushes and the directory become existence checks, fewer commands.
   - XCUITest (`ui`): the SDK platform path (1); `runner` without a test application: two copies, three architecture
     commands (`lipo` twice and a removal, on arm64e), three `PlistBuddy` edits, the `PlugIns` directory and copy (10),
     or with one: `chmod`, symlink and the architecture commands (5); the app's `chmod` (1): up to 12.
   - Permissions (`grant_permissions`): the SIP check and one `sqlite3` per permission, when there are grants.
   - Not counted: code coverage's static-library directories (one per library, known only from the built xctestrun),
     and every transfer (`push_shared`, `push_generated_file`), uncapped by D54.
6. *Integration test.* The real-time run asserts only the bound; the sequence is witnessed in virtual time.

The migration notes now say that a preparation failing at once keeps its 30 attempts with the default timeouts; shorter
timeouts and a shorter `deviceInitializationTimeoutMillis` can make the deadline shorter than those 290 s.

Each round-2 witness fails on 3f4bb7d9, checked in a scratch copy of that tree with the new tests; round 1's witnesses
pass there, as they should.

## Review round 3

1. *A refused transfer retry cancelled the boot beside it.* The Apple installer's gate records a refusal, and the
   iOS installation half reports it as `PrepareError::Refused`, which the preparation does not treat as a failure to
   cancel over: it lets the boot end on its own, then reports the transfer's failure. Every deadline refusal now stops
   nothing that runs: the actor's and the Android installer's have no sibling, the boot's (`OutOfTime`) and the
   transfer's (`Refused`) leave theirs to end. Witness: a replacement with 10 s left, `simctl boot` taking 25 s, the
   send failing at 20 s: the boot's command is not cancelled.
2. *A held read could spend the shutdown reserve.* `command_worst_case_millis` (next to the SSH limits) is a command's
   worst case: channel open and exec acknowledgment within 30 s each, its own limit, D30's 30 s. Every check of a held
   attempt uses it (a state read is two commands). Witness: 15 s boot, then 65 s reads and probes (20 + 20 + 25 s):
   every shutdown starts with its whole reserve, where one used to start 5 s into it.
3. *A worker drop during a replacement's preparation renewed the deadline.* The recovery carries the episode for any
   loss of its claim, a worker's included; only a removal or re-add starts afresh. Witness: replacement inherits the
   episode, its worker drops while `simctl boot` is pending, reconnect: the next setup continues the same deadline.
4. *macOS undercount.* The count is now measured, not read off the code: `tests/preparation_commands.rs` runs each
   variant's longest path on the fake worker (the installer's three attempts forced by two failed sends, an arm64e Mac
   whose runner has an extra architecture, the probe finding XCUITest) and compares the commands sent, every one under
   `shell`, with the budget. What it found beyond round 2's list: each permission writes twice (the system's and the
   user's TCC database), and each of the runner's two overwriting copies is preceded by a removal. Hosted XCTest with a
   permission: 15 commands, 452 s.

The commands one preparation can send, each under `shell` (round 2's list, corrected):

- `prepare_inner`: remove, create and create-shared directory (3).
- Apple installer, per attempt at the test bundle: create and create-shared directory (2 × 3).
- The test type's probe (`nm`), unless configured (1).
- Test root factory: the test root directory (1).
- Hosted XCTest: the app's `chmod`, its `PlugIns` directory (2).
- XCUITest without a runner of its own: the SDK platform path (1); removal and copy of `XCTRunner.app`, removal and copy
  of its binary (4); `lipo -archs` twice and `lipo -remove` on arm64e (3); three `PlistBuddy` edits (3); the runner's
  `PlugIns` directory and the test bundle's copy (2); the app's `chmod` (1): 14. With a runner of its own: `chmod`,
  removal and link, the architecture commands, and the SDK path and the app's `chmod`: 8.
- Permissions: `csrutil status` (1), and per permission two `sqlite3` writes (2 each).
- Not counted: code coverage's static-library directories (one per library, only the built xctestrun names them), and
  transfers (D54).

## Review round 4

The branch was rebased onto the final x82 (with x83's D85 and x84's D86 on main) before this round.

1. *The Android witness met D85.* After the actor's attempts end, a device still listed is set up once more (D85,
   `provider.rs`, `retry_failed_prepares`). That instance used to open a fresh deadline, so the witness saw three more
   installs (19 instead of 16). **Decision:** D85's one more setup continues the exhausted preparation's deadline
   (`AndroidDevice::continue_preparation`, handed over with `Replacement::PrepareFailed`). Its first attempt always runs,
   because the actor always makes its first attempt and the installer always makes each package's first attempt, so a
   device that really rebooted still gets ready; it makes no retries past the deadline, so D85 cannot double the time a
   failing device holds its place (deadline plus one attempt, instead of two whole budgets). A fresh episode would have
   been the alternative; it would let a device that kept failing slowly hold its place for a second full deadline, which
   is what D87 exists to stop. The witness now counts each instance: the first makes six preparations and is cut after 6
   attempts; the second, under the passed deadline, makes one install and is cut after 1 attempt; 2 + 14 + 1 installs,
   three installer refusals, the run ending within the deadline, one attempt and D48's wait.
2. *A refused retry could start a media upload.* A flag set when one half of an iOS preparation runs out of time (a boot
   out of time, a transfer retry refused) keeps the other half, left to finish what it is doing, from starting any
   further step: the media importer's uploads and imports, the second boot attempt, and the Apple installer's test root,
   after-install step and extra applications (`with_step_gate`). Witness: send fails at 20 s, refused; the boot ends at
   25 s; no media upload starts (one send in all).
3. *The shutdown's own commands could outlast the reserve.* A held attempt's bound is now the deadline less the
   shutdown's worst case, its descriptor read and `simctl shutdown` at `command_worst_case_millis` (240 s with the
   defaults), not `shutdown` + `shell`. The held attempt and its shutdown together end by the deadline. Witness: 120 s
   reads and probes and a 100 s `simctl shutdown` (cut at the shutdown's own 60 s limit, ending through its own): the
   preparation ends at about 1,340 s of a 1,500 s deadline, where it ended at about 1,581 s.
4. *macOS cache checks.* A destination with a recorded earlier transfer of the same file (from a parse or another device
   on the worker) is checked with `test -e` before it is sent again (`shared_transfers.rs`); retries after a failed send
   do not check again. Counted: one per shared destination, the test bundle, the app, hosted XCTest's `PlugIns` copy and
   each extra application. The measured-count test now warms the cache with an earlier preparation on the same worker
   and adds an extra-application variant: 27, 26, 18, 20 and 16 commands, each equal to its budget.

Follow-up, not changed here (D84 integration, from the reviewer): after the Scheduler's wait for the pools to stop
expires, the provider's termination can wait a second 70 s for its devices' disposal. Recorded for the orchestrator.

Each round-4 witness fails on d75a9ccc, checked in a scratch copy of that tree with the new tests.

## Incidental bugs

None found.
